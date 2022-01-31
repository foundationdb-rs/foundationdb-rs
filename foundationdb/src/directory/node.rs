use crate::directory::directory_layer::{
    DirectoryLayer, DEFAULT_SUB_DIRS, LAYER_SUFFIX, PARTITION_LAYER,
};
use crate::directory::error::DirectoryError;
use crate::directory::DirectoryOutput;
use crate::tuple::Subspace;
use crate::RangeOption;
use crate::Transaction;

#[derive(Debug, Clone)]
pub(super) struct Node {
    pub(super) subspace: Subspace,
    pub(super) current_path: Vec<String>,
    pub(super) target_path: Vec<String>,
    pub(super) directory_layer: DirectoryLayer,
    pub(super) layer: Vec<u8>,
}

impl Node {
    // `load_metadata` is loading extra information for the node, like the layer
    pub(crate) async fn load_metadata(
        trx: &Transaction,
        subspace: &Subspace,
    ) -> Result<Vec<u8>, DirectoryError> {
        let key = subspace.pack(&LAYER_SUFFIX);
        let layer = match trx.get(&key, false).await {
            Err(err) => return Err(DirectoryError::FdbError(err)),
            Ok(fdb_slice) => fdb_slice.as_deref().unwrap_or_default().to_vec(),
        };
        Ok(layer)
    }

    pub(crate) fn get_partition_subpath(&self) -> Vec<String> {
        Vec::from(&self.target_path[self.current_path.len()..])
    }

    /// list sub-folders for a node
    pub(crate) async fn list_sub_folders(
        &self,
        trx: &Transaction,
    ) -> Result<Vec<String>, DirectoryError> {
        let mut results = vec![];

        let range_option = RangeOption::from(&self.subspace.subspace(&DEFAULT_SUB_DIRS));

        let fdb_values = trx.get_range(&range_option, 1_024, false).await?;

        for fdb_value in fdb_values {
            let subspace = Subspace::from_bytes(fdb_value.key());
            // stripping from subspace
            let sub_directory: (i64, String) = self.subspace.unpack(subspace.bytes())?;
            results.push(sub_directory.1);
        }
        Ok(results)
    }

    pub(crate) fn is_in_partition(&self, include_empty_subpath: bool) -> bool {
        self.layer.as_slice().eq(PARTITION_LAYER)
            && (include_empty_subpath || self.target_path.len() > self.current_path.len())
    }

    pub(crate) fn get_contents(&self) -> Result<DirectoryOutput, DirectoryError> {
        self.directory_layer
            .contents_of_node(&self.subspace, &self.current_path, &self.layer)
    }
}
