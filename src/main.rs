fn main() {
    let mut args = std::env::args_os().skip(1);
    let nx_file_path = args.next().expect("Need path to nx file as first argument.");
    let mount_path = args.next().expect("Need mount path as second argument.");
}
