use crate::directory::directory_layer::{
    DirectoryLayer, DEFAULT_SUB_DIRS, LAYER_SUFFIX, PARTITION_LAYER,
};
use crate::directory::error::DirectoryError;
use crate::directory::DirectoryOutput;
use crate::tuple::Subspace;
use crate::RangeOption;
use crate::Transaction;

#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub(crate) subspace: Option<Subspace>,
    pub(crate) current_path: Vec<String>,
    pub(crate) target_path: Vec<String>,
    pub(crate) layer: Vec<u8>,
    pub(crate) loaded_metadata: bool,
    pub(crate) directory_layer: DirectoryLayer,
}

impl Node {
    // `load_metadata` is loading extra information for the node, like the layer
    pub(crate) async fn load_metadata(&mut self, trx: &Transaction) -> Result<(), DirectoryError> {
        if !self.exists() {
            self.loaded_metadata = true;
            return Ok(());
        }

        let key = self.subspace.as_ref().unwrap().pack(&LAYER_SUFFIX);
        self.layer = match trx.get(&key, false).await {
            Ok(None) => vec![],
            Err(err) => return Err(DirectoryError::FdbError(err)),
            Ok(Some(fdb_slice)) => fdb_slice.to_vec(),
        };

        self.loaded_metadata = true;

        Ok(())
    }

    pub(crate) fn is_in_partition(&self, include_empty_subpath: bool) -> bool {
        assert!(self.loaded_metadata);

        self.exists()
            && self.layer.as_slice().eq(PARTITION_LAYER)
            && (include_empty_subpath || self.target_path.len() > self.current_path.len())
    }

    pub(crate) fn get_partition_subpath(&self) -> Vec<String> {
        Vec::from(&self.target_path[self.current_path.len()..])
    }

    pub(crate) fn exists(&self) -> bool {
        self.subspace.is_some()
    }

    /// list sub-folders for a node
    pub(crate) async fn list_sub_folders(
        &self,
        trx: &Transaction,
    ) -> Result<Vec<String>, DirectoryError> {
        let mut results = vec![];

        let range_option = RangeOption::from(
            &self
                .subspace
                .as_ref()
                .unwrap()
                .subspace(&(DEFAULT_SUB_DIRS)),
        );

        let fdb_values = trx.get_range(&range_option, 1_024, false).await?;

        for fdb_value in fdb_values {
            let subspace = Subspace::from_prefix_key(fdb_value.key());
            // stripping from subspace
            let sub_directory: (i64, String) =
                self.subspace.as_ref().unwrap().unpack(subspace.bytes())?;
            results.push(sub_directory.1);
        }
        Ok(results)
    }

    pub(crate) fn get_contents(&self) -> Result<DirectoryOutput, DirectoryError> {
        assert!(self.exists());
        assert!(self.loaded_metadata);

        match &self.subspace {
            None => unreachable!(),
            Some(subspace) => {
                self.directory_layer
                    .contents_of_node(subspace, &self.current_path, &self.layer)
            }
        }
    }
}
