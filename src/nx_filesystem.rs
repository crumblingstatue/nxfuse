use std::path::Path;

use nx;

pub struct NxFilesystem {
    nx_file: nx::File
}

impl NxFilesystem {
    pub fn open_nx_file(path: &Path) -> Result<NxFilesystem, nx::Error> {
        Ok(NxFilesystem {
            nx_file: try!(nx::File::open(path))
        })
    }
}
