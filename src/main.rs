extern crate nx;
extern crate fuse;

mod nx_filesystem;

use nx_filesystem::NxFilesystem;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let nx_file_path = args.next().expect("Need path to nx file as first argument.");
    let mount_path = args.next().expect("Need mount path as second argument.");
    let fs = NxFilesystem::open_nx_file(nx_file_path.as_ref())
                 .unwrap_or_else(|e| panic!("Can't open nx file: {}", e));
    fuse::mount(fs, &mount_path, &[]);
}
