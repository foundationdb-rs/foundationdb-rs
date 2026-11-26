//! The directory layer instructions.

use foundationdb as fdb;

use std::borrow::Cow;
use std::sync::Arc;

use fdb::tuple::{Bytes, Element, Subspace, TuplePack, pack, unpack};
use fdb::*;
use futures::future;
use futures::prelude::*;

use foundationdb::directory::DirectoryError;
use foundationdb::directory::DirectoryLayer;
use foundationdb::directory::{Directory, DirectoryOutput};
use foundationdb::tuple::{PackResult, TupleUnpack};

use crate::{Instr, StackMachine, TransactionState};

#[derive(Debug)]
pub(crate) enum DirectoryStackItem {
    Directory(DirectoryLayer),
    DirectoryOutput(DirectoryOutput),
    Subspace(Subspace),
    Null,
}

pub(crate) async fn directory_operation<D, T, F>(
    db: &Database,
    transaction: &TransactionState,
    is_db: bool,
    data: D,
    operation: F,
) -> Result<T, FdbBindingError>
where
    F: for<'a> Fn(&'a Transaction, &'a D) -> future::LocalBoxFuture<'a, Result<T, DirectoryError>>,
{
    if is_db {
        // Retry only the directory operation and commit. Stack arguments have already
        // been popped, and the caller publishes the result once this succeeds.
        db.run(|txn, _| {
            let data = &data;
            let operation = &operation;
            async move { operation(&txn, data).await.map_err(FdbBindingError::from) }
        })
        .await
    } else {
        let TransactionState::Transaction(txn) = transaction else {
            panic!("could not find an active transaction");
        };
        operation(txn, &data).await.map_err(FdbBindingError::from)
    }
}

impl StackMachine {
    /// Runs one `DIRECTORY_*` instruction. Errors are pushed on the stack.
    #[allow(clippy::cognitive_complexity)]
    pub(crate) async fn run_directory_instr(
        &mut self,
        db: Arc<Database>,
        number: usize,
        instr: &mut Instr,
        is_db: bool,
        trx: &mut TransactionState,
    ) {
        use crate::InstrCode::*;

        match instr.code {
            // Pop 1 tuple off the stack as [path]. Pop 1 additional item as [raw_prefix].
            // Create a subspace with path as the prefix tuple and the specified
            // raw_prefix. Append it to the directory list.
            DirectoryCreateSubspace => {
                let tuple_prefix = self.pop_string_tuple(1).await;
                let raw_prefix = self.pop_bytes().await;
                let subspace = Subspace::from_bytes(raw_prefix.into_owned())
                    .subspace(tuple_prefix.first().unwrap());
                debug!(
                    "pushing a new subspace {:?} at index {}",
                    subspace,
                    self.directory_stack.len()
                );
                self.directory_stack
                    .push(DirectoryStackItem::Subspace(subspace));
            }

            // Pop 3 items off the stack as [index1, index2, allow_manual_prefixes]. Let
            // node_subspace be the object in the directory list at index1 and
            // content_subspace be the object in the directory list at index2. Create a new
            // directory layer with the specified node_subspace and content_subspace. If
            // allow_manual_prefixes is 1, then enable manual prefixes on the directory
            // layer. Append the resulting directory layer to the directory list.
            //
            // If either of the two specified subspaces are null, then do not create a
            // directory layer and instead push null onto the directory list.
            DirectoryCreateLayer => {
                let index_node_subspace = self.pop_usize().await;
                let index_content_subspace = self.pop_usize().await;
                let allow_manual_prefixes = self.pop_i64().await == 1;

                let node_subspace = self.directory_stack.get(index_node_subspace);
                let content_subspace = self.directory_stack.get(index_content_subspace);

                match (node_subspace, content_subspace) {
                    (Some(node_subspace), Some(content_subspace)) => {
                        let node_subspace = match node_subspace {
                            DirectoryStackItem::Subspace(s) => s.to_owned(),
                            _ => panic!("expecting subspace"),
                        };

                        let content_subspace = match content_subspace {
                            DirectoryStackItem::Subspace(s) => s.to_owned(),
                            _ => panic!("expecting subspace"),
                        };

                        debug!("pushed a directory at index {}", self.directory_stack.len());

                        self.directory_stack.push(DirectoryStackItem::Directory(
                            DirectoryLayer::new(
                                node_subspace,
                                content_subspace,
                                allow_manual_prefixes,
                            ),
                        ));
                    }
                    (node_subspace, content_subspace) => {
                        error!(
                            "pushing null on the directory list: {}, {}",
                            node_subspace.is_some(),
                            content_subspace.is_some()
                        );
                        self.directory_stack.push(DirectoryStackItem::Null);
                    }
                }
            }

            // Pop 1 tuple off the stack as [path]. Pop 2 additional items as
            // [layer, prefix]. create a directory with the specified path, layer,
            // and prefix. If either of layer or prefix is null, use the default value for
            // that parameter (layer='', prefix=null).
            DirectoryCreate => {
                let path = self.pop_string_tuple(1).await;
                let layer = self.pop_optional_bytes().await;
                let prefix = self.pop_optional_bytes().await;

                let directory = self
                    .get_current_directory()
                    .expect("could not find a directory");

                debug!(
                    "creating path {:?} with layer {:?} and prefix {:?} using directory at index {}",
                    path.first().unwrap(),
                    layer,
                    prefix,
                    self.directory_index,
                );

                match directory_operation(
                    &db,
                    &*trx,
                    is_db,
                    (directory, path, prefix, layer),
                    |txn, (directory, path, prefix, layer)| {
                        directory
                            .create(
                                txn,
                                path.first().unwrap(),
                                prefix.as_deref(),
                                layer.as_deref(),
                            )
                            .boxed_local()
                    },
                )
                .await
                {
                    Ok(directory_subspace) => {
                        debug!(
                            "pushing DirectoryOutput at index {}",
                            self.directory_stack.len()
                        );
                        self.directory_stack
                            .push(DirectoryStackItem::DirectoryOutput(directory_subspace));
                    }
                    Err(e) => {
                        self.push_directory_err(&instr.code, number, e);
                    }
                };
            }

            // Pop 1 tuple off the stack as [path]. Pop 1 additional item as [layer]. Open
            // a directory with the specified path and layer. If layer is null, use the
            // default value (layer='').
            DirectoryOpen => {
                let path = self.pop_string_tuple(1).await;
                // The layer may be null, an empty layer means the default one too.
                let layer = self.pop_optional_bytes().await.filter(|l| !l.is_empty());

                let directory = self
                    .get_current_directory()
                    .expect("could not find a directory");

                debug!(
                    "opening path {:?} with layer {:?} with index {}",
                    path.first().unwrap(),
                    layer,
                    self.directory_index
                );

                let snapshot = instr.pop_snapshot();
                match directory_operation(
                    &db,
                    &*trx,
                    is_db,
                    (directory, path, layer),
                    |txn, (directory, path, layer)| {
                        directory
                            .open(txn, path.first().unwrap(), layer.as_deref(), snapshot)
                            .boxed_local()
                    },
                )
                .await
                {
                    Ok(directory_subspace) => {
                        debug!(
                            "pushing newly opened DirectoryOutput at index {}",
                            self.directory_stack.len()
                        );
                        self.directory_stack
                            .push(DirectoryStackItem::DirectoryOutput(directory_subspace));
                    }
                    Err(e) => {
                        self.push_directory_err(&instr.code, number, e);
                    }
                };
            }

            // Use the current directory for this operation.
            //
            // Pop 1 tuple off the stack as [path]. Pop 1 additional item as [layer]. Open
            // a directory with the specified path and layer. If layer is null, use the
            // default value (layer='').
            DirectoryCreateOrOpen => {
                let path = self.pop_string_tuple(1).await;
                // The layer may be null, an empty layer means the default one too.
                let layer = self.pop_optional_bytes().await.filter(|l| !l.is_empty());

                let directory = self
                    .get_current_directory()
                    .expect("could not find a directory");

                debug!(
                    "create_or_open path {:?} with layer {:?} with index {}",
                    path.first().unwrap(),
                    layer,
                    self.directory_index
                );
                match directory_operation(
                    &db,
                    &*trx,
                    is_db,
                    (directory, path, layer),
                    |txn, (directory, path, layer)| {
                        directory
                            .create_or_open(txn, path.first().unwrap(), None, layer.as_deref())
                            .boxed_local()
                    },
                )
                .await
                {
                    Ok(directory_subspace) => {
                        debug!(
                            "pushing created_or_opened {:?} at index {}",
                            directory_subspace,
                            self.directory_stack.len()
                        );
                        self.directory_stack
                            .push(DirectoryStackItem::DirectoryOutput(directory_subspace));
                    }
                    Err(e) => self.push_directory_err(&instr.code, number, e),
                };
            }

            // Pop the top item off the stack as [index]. Set the current directory list
            // index to index. In the event that the directory at this new index is null
            // (as the result of a previous error), set the directory list index to the
            // error index.
            DirectoryChange => {
                self.directory_index = self.pop_usize().await;
                debug!("setting directory_index to {}", self.directory_index);
                match self.directory_stack.get(self.directory_index) {
                    None => {
                        self.directory_index = self.error_index;
                        debug!(
                            "setting directory_index to error index {}: no directory found",
                            self.directory_index
                        );
                    }
                    Some(d) => {
                        if let DirectoryStackItem::Null = d {
                            self.directory_index = self.error_index;
                            debug!(
                                "setting directory_index to error index {}: because it is Null",
                                self.directory_index
                            );
                        }
                    }
                }
            }

            // Pop the top item off the stack as [error_index]. Set the current error index
            // to error_index.
            DirectorySetErrorIndex => {
                self.error_index = self.pop_usize().await;
            }

            // Use the current directory for this operation.
            //
            // Pop 2 tuples off the stack as [old_path, new_path]. Call move with the
            // specified old_path and new_path. Append the result onto the directory list.
            DirectoryMove => {
                let paths = self.pop_string_tuple(2).await;

                let directory = self
                    .get_current_directory()
                    .expect("could not find a directory");

                debug!(
                    "moving {:?} to {:?} using directory at index {}",
                    paths.first().unwrap(),
                    paths.get(1).unwrap(),
                    self.directory_index
                );

                match directory_operation(
                    &db,
                    &*trx,
                    is_db,
                    (directory, paths),
                    |txn, (directory, paths)| {
                        directory
                            .move_to(txn, paths.first().unwrap(), paths.get(1).unwrap())
                            .boxed_local()
                    },
                )
                .await
                {
                    Ok(s) => {
                        debug!(
                            "pushing moved directory {:?} at index {}",
                            s,
                            self.directory_stack.len()
                        );
                        self.directory_stack
                            .push(DirectoryStackItem::DirectoryOutput(s));
                    }
                    Err(e) => {
                        self.push_directory_err(&instr.code, number, e);
                    }
                };
            }

            // Use the current directory for this operation.
            //
            // Pop 1 tuple off the stack as [new_absolute_path]. Call moveTo with the
            // specified new_absolute_path. Append the result onto the directory list.
            DirectoryMoveTo => {
                let paths = self.pop_string_tuple(1).await;

                let directory = self
                    .get_current_directory()
                    .expect("could not find a directory");

                debug!("moving directory {:?} to {:?}", self.directory_index, paths);

                match directory_operation(
                    &db,
                    &*trx,
                    is_db,
                    (directory, paths),
                    |txn, (directory, paths)| {
                        directory
                            .move_directory(txn, paths.first().expect("popped tuple has no item"))
                            .boxed_local()
                    },
                )
                .await
                {
                    Ok(s) => {
                        debug!(
                            "pushing moved directory {:?} at index {}",
                            s,
                            self.directory_stack.len()
                        );
                        self.directory_stack
                            .push(DirectoryStackItem::DirectoryOutput(s));
                    }
                    Err(e) => {
                        self.push_directory_err(&instr.code, number, e);
                    }
                };
            }

            // Use the current directory for this operation.
            //
            // Pop 1 item off the stack as [count] (either 0 or 1). If count is 1, pop 1
            // tuple off the stack as [path]. Call remove_if_exits, passing it path if one
            // was popped.
            DirectoryRemove => {
                let count = self.pop_usize().await;
                let paths = self.pop_string_tuple(count).await;

                let directory = self
                    .get_current_directory()
                    .expect("could not find a directory");

                let paths = paths.first().expect("could not retrieve a path");
                debug!(
                    "removing path {:?} using directory at index {}",
                    paths, self.directory_index
                );
                match directory_operation(
                    &db,
                    &*trx,
                    is_db,
                    (directory, paths),
                    |txn, (directory, paths)| {
                        async move {
                            if directory.remove(txn, paths).await? {
                                Ok(())
                            } else {
                                Err(DirectoryError::DirectoryDoesNotExists)
                            }
                        }
                        .boxed_local()
                    },
                )
                .await
                {
                    Ok(()) => {}
                    Err(e) => {
                        self.push_directory_err(&instr.code, number, e);
                    }
                }
            }

            // Use the current directory for this operation.
            //
            // Pop 1 item off the stack as [count] (either 0 or 1). If count is 1, pop 1
            // tuple off the stack as [path]. Call remove_if_exits, passing it path if one
            // was popped.
            DirectoryRemoveIfExists => {
                let count = self.pop_usize().await;
                let paths = self.pop_string_tuple(count).await;

                let directory = self
                    .get_current_directory()
                    .expect("could not find a directory");

                let paths = paths.first().expect("could not retrieve a path");
                match directory_operation(
                    &db,
                    &*trx,
                    is_db,
                    (directory, paths),
                    |txn, (directory, paths)| directory.remove_if_exists(txn, paths).boxed_local(),
                )
                .await
                {
                    Ok(_) => {}
                    Err(err) => self.push_directory_err(&instr.code, number, err),
                };
            }

            // Use the current directory for this operation.
            //
            // Pop 1 item off the stack as [count] (either 0 or 1). If count is 1, pop 1
            // tuple off the stack as [path]. Call list, passing it path if one was popped.
            // Pack the resulting list of directories using the tuple layer and push the
            // packed string onto the stack.
            DirectoryList => {
                let count = self.pop_usize().await;
                let paths = match count {
                    1 => {
                        let paths = self.pop_string_tuple(count).await;
                        paths.first().expect("could not retrieve a path").clone()
                    }
                    0 => vec![],
                    _ => panic!(),
                };

                let directory = self
                    .get_current_directory()
                    .expect("could not find a directory");

                let snapshot = instr.pop_snapshot();
                match directory_operation(
                    &db,
                    &*trx,
                    is_db,
                    (directory, &paths),
                    |txn, (directory, paths)| directory.list(txn, paths, snapshot).boxed_local(),
                )
                .await
                {
                    Ok(children) => {
                        let mut elements: Vec<Element> = vec![];
                        debug!(
                            "found {} items under path {:?} with directory at index {}:",
                            children.len(),
                            paths,
                            self.directory_index
                        );
                        for child in children {
                            debug!("\t - {}", child);
                            let element = Element::String(Cow::from(child));
                            elements.push(element);
                        }
                        let tuple = Element::Tuple(elements);
                        self.push(number, Element::Bytes(Bytes::from(tuple.pack_to_vec())));
                    }
                    Err(e) => {
                        self.push_directory_err(&instr.code, number, e);
                    }
                };
            }

            // Use the current directory for this operation.
            //
            // Pop 1 item off the stack as [count] (either 0 or 1). If count is 1, pop 1
            // tuple off the stack as [path]. Call exists, passing it path if one
            // was popped. Push 1 onto the stack if the path exists and 0 if it does not.
            DirectoryExists => {
                let count = self.pop_usize().await;

                let paths = self.pop_string_tuple(count).await;

                let directory = self
                    .get_current_directory()
                    .expect("could not find a directory");

                let paths = paths.first().expect("could not retrieve a path");
                let snapshot = instr.pop_snapshot();
                match directory_operation(
                    &db,
                    &*trx,
                    is_db,
                    (directory, paths),
                    |txn, (directory, paths)| directory.exists(txn, paths, snapshot).boxed_local(),
                )
                .await
                {
                    Ok(exists) => {
                        self.push(number, Element::Int(i64::from(exists)));
                    }
                    Err(e) => {
                        self.push_directory_err(&instr.code, number, e);
                    }
                };
            }

            // Use the current directory for this operation.
            //
            // Pop 1 tuple off the stack as [key_tuple]. Pack key_tuple and push the result
            // onto the stack.
            DirectoryPackKey => {
                let n: usize = self.pop_usize().await;
                debug!("DirectoryPackKey {n}");
                let mut buf = Vec::with_capacity(n);
                for _ in 0..n {
                    let element: Element = self.pop_element().await;
                    debug!(" - {element:?}");
                    buf.push(element);
                }

                let tuple = Element::Tuple(buf);

                match self.pack_with_current_subspace(&tuple) {
                    None => self.push_directory_err(
                        &instr.code,
                        number,
                        DirectoryError::Other(String::from("cannot pack with current item")),
                    ),
                    Some(bytes) => {
                        self.push(number, Element::Bytes(bytes.into()));
                    }
                }
            }

            // Use the current directory for this operation.
            //
            // Pop 1 item off the stack as [key]. Unpack key and push the resulting tuple
            // onto the stack one item at a time.
            DirectoryUnpackKey => {
                let data = self.pop_bytes().await;
                let data = data.to_vec();
                debug!("directory_unpack {data:?}");
                match self.unpack_with_current_subspace(&data) {
                    None => self.push_directory_err(
                        &instr.code,
                        number,
                        DirectoryError::Other(String::from(
                            "Cannot unpack with Directory Partition",
                        )),
                    ),
                    Some(packed) => {
                        let data: Vec<Element> = packed.unwrap();
                        for element in data {
                            debug!(" - {element:?}");
                            self.push(number, Element::Tuple(vec![element.into_owned()]));
                        }
                    }
                }
            }

            // Use the current directory for this operation.
            //
            // Pop 1 tuple off the stack as [tuple]. Create a range using tuple and push
            // range.begin and range.end onto the stack.
            DirectoryRange => {
                let n: usize = self.pop_usize().await;
                let mut buf = Vec::with_capacity(n);
                for _ in 0..n {
                    let element: Element = self.pop_element().await;
                    debug!(" - {element:?}");
                    buf.push(element);
                }

                match self.get_current_directory_item() {
                    Some(DirectoryStackItem::DirectoryOutput(
                        DirectoryOutput::DirectoryPartition(_d),
                    )) => {
                        self.push_directory_err(
                            &instr.code,
                            number,
                            DirectoryError::Other(String::from(
                                "operation not allowed on directoryPartition",
                            )),
                        );
                    }
                    _ => {
                        let tuple = Element::Tuple(buf);
                        match self.subspace_with_current_item(&tuple) {
                            None => {
                                self.push_directory_err(
                                    &instr.code,
                                    number,
                                    DirectoryError::Other(String::from(
                                        "operation not allowed on directoryPartition",
                                    )),
                                );
                            }
                            Some(subspace) => {
                                let (begin_range, end_range) = subspace.range();
                                self.push(number, Element::Bytes(begin_range.into()));
                                self.push(number, Element::Bytes(end_range.into()));
                            }
                        }
                    }
                }
            }

            // Use the current directory for this operation.
            //
            // Pop 1 item off the stack as [key]. Check if the current directory contains
            // the specified key. Push 1 if it does and 0 if it doesn't.
            DirectoryContains => {
                let raw_prefix = self.pop_bytes().await;
                let maybe_contains = match self.get_current_directory_item() {
                    None => None,
                    Some(DirectoryStackItem::Subspace(s)) => Some(s.is_start_of(&raw_prefix)),
                    Some(DirectoryStackItem::DirectoryOutput(
                        DirectoryOutput::DirectorySubspace(s),
                    )) => Some(s.is_start_of(&raw_prefix)),
                    _ => None,
                };
                match maybe_contains {
                    None => self.push_directory_err(
                        &instr.code,
                        number,
                        DirectoryError::Other(String::from("cannot perform contains")),
                    ),
                    Some(bool) => self.push(number, Element::Int(bool as i64)),
                }
            }

            // Use the current directory for this operation.
            //
            // Pop 1 tuple off the stack as [tuple]. Open the subspace of the current
            // directory specified by tuple and push it onto the directory list.
            DirectoryOpenSubspace => {
                let n: usize = self.pop_usize().await;
                debug!("DirectoryRange {n}");
                let mut buf = Vec::with_capacity(n);
                for _ in 0..n {
                    let element: Element = self.pop_element().await;
                    debug!(" - {element:?}");
                    buf.push(element);
                }

                let tuple = Element::Tuple(buf);
                match self.subspace_with_current_item(&tuple) {
                    None => self.push_directory_err(
                        &instr.code,
                        number,
                        DirectoryError::Other(String::from("Cannot subspace with current item")),
                    ),
                    Some(s) => self.directory_stack.push(DirectoryStackItem::Subspace(s)),
                }
            }

            // Use the current directory for this operation.
            //
            // Pop 1 item off the stack as [prefix]. Let key equal
            // prefix + tuple.pack([dir_index]). Set key to be the result of calling
            // directory.key() in the current transaction.
            DirectoryLogSubspace => {
                debug!(
                    "logging subspace {}/{}",
                    self.directory_index,
                    self.directory_stack.len()
                );
                let raw_prefix = self.pop_bytes().await;
                let txn = match *trx {
                    TransactionState::Transaction(ref t) => t,
                    _ => {
                        panic!("could not find an active transaction");
                    }
                };
                let key = Subspace::from_bytes(raw_prefix.into_owned()).pack(&self.directory_index);

                match self.directory_stack.get(self.directory_index) {
                    None => panic!("nothing in the stack"),
                    Some(DirectoryStackItem::Null) => panic!("Directory is NULL"),
                    Some(DirectoryStackItem::Directory(_)) => {
                        panic!("trying to get a subspace, got a Directory")
                    }
                    Some(DirectoryStackItem::DirectoryOutput(
                        DirectoryOutput::DirectorySubspace(d),
                    )) => {
                        txn.set(&key, d.bytes());
                        debug!(
                            "logging subspace [{}] {:?}={:?}",
                            self.directory_index,
                            unpack::<Vec<Element>>(&key).unwrap(),
                            d.bytes(),
                        );
                    }
                    Some(DirectoryStackItem::DirectoryOutput(
                        DirectoryOutput::DirectoryPartition(_),
                    )) => {
                        self.push_directory_err(
                            &instr.code,
                            number,
                            DirectoryError::Other(String::from(
                                "cannot get key for the root of a directory partition",
                            )),
                        );
                    }
                    Some(DirectoryStackItem::Subspace(s)) => {
                        txn.set(&key, s.bytes());
                        debug!(
                            "logging subspace [{}] {:?}={:?}",
                            self.directory_index,
                            unpack::<Vec<Element>>(&key).unwrap(),
                            s.bytes(),
                        );
                    }
                };
            }

            // Use the current directory for this operation.
            //
            // Pop 1 item off the stack as [raw_prefix]. Create a subspace log_subspace
            // with path (dir_index) and the specified raw_prefix. Set:
            //
            // tr[log_subspace[u'path']] = the tuple packed path of the directory.
            //
            // tr[log_subspace[u'layer']] = the tuple packed layer of the directory.
            //
            // tr[log_subspace[u'exists']] = the packed tuple containing a 1 if the
            // directory exists and 0 if it doesn't.
            //
            // tr[log_subspace[u'children']] the tuple packed list of children of the
            // directory.
            //
            // Where log_subspace[u<str>] is the subspace packed tuple containing only the
            // single specified unicode string <str>.
            DirectoryLogDirectory => {
                debug!("logging directory {}", self.directory_index);
                let directory = self
                    .get_current_directory()
                    .expect("could not find a directory");

                let txn = match *trx {
                    TransactionState::Transaction(ref t) => t,
                    _ => {
                        panic!("could not find an active transaction");
                    }
                };

                let raw_prefix = self.pop_bytes().await;
                let subspace =
                    Subspace::from_bytes(raw_prefix.into_owned()).subspace(&self.directory_index);

                let key_path = subspace.pack(&"path");
                let value_path = pack(&self.get_path_for_current_directory().unwrap());

                let key_layer = subspace.pack(&"layer");
                let value_layer = pack(&self.get_layer_for_current_directory().unwrap());

                let exists = directory
                    .exists(txn, &[], false)
                    .await
                    .expect("could not list directory");

                let key_exists = subspace.pack(&"exists");
                let value_exists = pack(&Element::Tuple(vec![Element::Int(exists.into())]));

                let children = if exists {
                    directory.list(txn, &[], false).await.unwrap()
                } else {
                    vec![]
                };

                let tuple_children = Element::Tuple(
                    children
                        .iter()
                        .map(|s| Element::String(Cow::Owned(s.clone())))
                        .collect(),
                );

                let key_children = subspace.pack(&"children");
                let value_children = pack(&tuple_children);

                txn.set(&key_path, &value_path);
                debug!(
                    "[{}] {:?}={:?}",
                    self.directory_index,
                    unpack::<Vec<Element>>(&key_path).unwrap(),
                    unpack::<Vec<Element>>(&value_path).unwrap(),
                );
                txn.set(&key_layer, &value_layer);
                debug!(
                    "[{}] {:?}={:?}",
                    self.directory_index,
                    unpack::<Vec<Element>>(&key_layer).unwrap(),
                    unpack::<Vec<Element>>(&value_layer).unwrap(),
                );
                txn.set(&key_exists, &value_exists);
                debug!(
                    "[{}] {:?}={:?}",
                    self.directory_index,
                    unpack::<Vec<Element>>(&key_exists).unwrap(),
                    unpack::<Vec<Element>>(&value_exists).unwrap(),
                );
                txn.set(&key_children, &value_children);
                debug!(
                    "[{}] {:?}={:?}",
                    self.directory_index,
                    unpack::<Vec<Element>>(&key_children).unwrap(),
                    unpack::<Vec<Element>>(&value_children).unwrap(),
                );
            }

            // Use the current directory for this operation.
            //
            // Pop 1 item off the stack as [byte_array]. Call .key() on the current
            // subspace and store the result as [prefix]. Throw an error if the popped
            // array does not start with prefix. Otherwise, remove the prefix from the
            // popped array and push the result onto the stack.
            DirectoryStripPrefix => match self.pop_optional_bytes().await {
                None => {
                    self.push_directory_err(
                        &instr.code,
                        number,
                        DirectoryError::Other(String::from("bad input on bytes")),
                    );
                }
                Some(raw_prefix) => match self.get_bytes_for_current_directory() {
                    None => self.push_directory_err(
                        &instr.code,
                        number,
                        DirectoryError::Version(String::from(
                            "cannot get bytes for DirectoryPartition",
                        )),
                    ),
                    Some(ssb) => {
                        if !raw_prefix.to_vec().starts_with(&ssb) {
                            self.push_directory_err(
                                &instr.code,
                                number,
                                DirectoryError::Version(String::from(
                                    "String does not start with raw prefix",
                                )),
                            );
                        } else {
                            self.push(
                                number,
                                Element::Bytes(Bytes::from(raw_prefix[ssb.len()..].to_owned())),
                            );
                        }
                    }
                },
            },
            ref code => unreachable!("{code:?} is not a directory instruction"),
        }
    }

    pub(crate) fn get_current_directory_item(&mut self) -> Option<&DirectoryStackItem> {
        self.directory_stack.get(self.directory_index)
    }

    pub(crate) fn get_current_directory(&mut self) -> Option<Box<dyn Directory>> {
        match self.get_current_directory_item() {
            None => None,
            Some(directory_or_subspace) => match directory_or_subspace {
                DirectoryStackItem::Directory(d) => Some(Box::new(d.clone())),
                DirectoryStackItem::DirectoryOutput(d) => Some(Box::new((*d).clone())),
                _ => None,
            },
        }
    }

    /// The current directory stack item as a subspace: a plain subspace or a directory
    /// subspace. None for a directory layer or a partition, which cannot be used as subspaces.
    fn current_subspace(&self) -> Option<Subspace> {
        match self.directory_stack.get(self.directory_index)? {
            DirectoryStackItem::DirectoryOutput(DirectoryOutput::DirectorySubspace(s)) => {
                Some(Subspace::from_bytes(s.bytes()))
            }
            DirectoryStackItem::Subspace(s) => Some(s.clone()),
            _ => None,
        }
    }

    pub(crate) fn pack_with_current_subspace<T: TuplePack>(&self, v: &T) -> Option<Vec<u8>> {
        self.current_subspace().map(|s| s.pack(v))
    }

    pub(crate) fn unpack_with_current_subspace<'de, T: TupleUnpack<'de>>(
        &self,
        key: &'de [u8],
    ) -> Option<PackResult<T>> {
        self.current_subspace().map(|s| s.unpack(key))
    }

    pub(crate) fn subspace_with_current_item<T: TuplePack>(&self, t: &T) -> Option<Subspace> {
        self.current_subspace().map(|s| s.subspace(t))
    }

    pub(crate) fn get_path_for_current_directory(&self) -> Option<&[String]> {
        match self.directory_stack.get(self.directory_index) {
            None => None,
            Some(directory_or_subspace) => match directory_or_subspace {
                DirectoryStackItem::Directory(d) => Some(d.get_path()),
                DirectoryStackItem::DirectoryOutput(d) => Some(d.get_path()),
                _ => None,
            },
        }
    }

    pub(crate) fn get_layer_for_current_directory(&self) -> Option<&[u8]> {
        match self.directory_stack.get(self.directory_index) {
            None => None,
            Some(directory_or_subspace) => match directory_or_subspace {
                DirectoryStackItem::DirectoryOutput(d) => Some(d.get_layer()),
                DirectoryStackItem::Directory(_) => Some(&[]),
                _ => None,
            },
        }
    }

    pub(crate) fn get_bytes_for_current_directory(&self) -> Option<Vec<u8>> {
        self.current_subspace().map(|s| s.bytes().to_vec())
    }
}
