use fuse::{Filesystem, Request, ReplyEntry, ReplyDirectory, ReplyData, ReplyAttr, FileType,
           FileAttr, FUSE_ROOT_ID};
use std::path::{Path, Component};
use libc::ENOENT;
use nx::{self, GenericNode};
use time::{self, Timespec};

pub struct NxFilesystem<'a> {
    nx_file: &'a nx::File,
    entries: Entries<'a>,
    /// Used to assign a new unique inode to nx nodes that don't have one yet
    inode_counter: u64,
    create_time: Timespec,
}

struct Entry<'a> {
    inode: u64,
    /// Optional data inode for entries that have both data, and children.
    ///
    /// In such cases there is a folder with the name of the node (e.g. `foo`), and
    /// a file with the data and _data appended (e.g. `foo_data`).
    ///
    /// This field represents the inode for that optional "foo_data" file.
    opt_data_inode: Option<u64>,
    node: nx::Node<'a>,
}

struct Entries<'a> {
    vec: Vec<Entry<'a>>,
}

impl<'a> Entries<'a> {
    fn new() -> Self {
        Entries { vec: Vec::new() }
    }
    fn push(&mut self, pair: Entry<'a>) {
        self.vec.push(pair);
    }
    fn node(&self, inode: u64) -> Option<nx::Node<'a>> {
        match self.vec.iter().find(|p| p.inode == inode) {
            Some(pair) => Some(pair.node),
            None => None,
        }
    }
    fn inode(&self, node: nx::Node) -> Option<u64> {
        match self.vec.iter().find(|p| p.node == node) {
            Some(pair) => Some(pair.inode),
            None => None,
        }
    }
}

fn with_node_data<R, T: FnOnce(&[u8]) -> R>(node: nx::Node, func: T) -> R {
    match node.dtype() {
        nx::Type::Empty => func(&[]),
        nx::Type::Integer => func(&node.integer().unwrap().to_string().as_bytes()),
        nx::Type::Float => func(&node.float().unwrap().to_string().as_bytes()),
        nx::Type::String => func(node.string().unwrap().as_bytes()),
        nx::Type::Vector => {
            let (x, y) = node.vector().unwrap();
            func(format!("({}, {})", x, y).as_bytes())
        }
        nx::Type::Bitmap => func(b"Reading bitmap nodes is not yet implemented, sorry :/"),
        nx::Type::Audio => func(node.audio().unwrap().data()),
    }
}

impl<'a> NxFilesystem<'a> {
    pub fn new_with_nx_file(nx_file: &'a nx::File) -> Self {
        let pairs = Entries::new();
        let mut fs = NxFilesystem {
            nx_file: nx_file,
            entries: pairs,
            inode_counter: FUSE_ROOT_ID + 1,
            create_time: time::get_time(),
        };
        // Add root node
        fs.entries.push(Entry { inode: FUSE_ROOT_ID, node: fs.nx_file.root(),
                                opt_data_inode: None });
        fs
    }
    fn new_inode(&mut self) -> u64 {
        let inode = self.inode_counter;
        self.inode_counter += 1;
        inode
    }
    /// Get inode for a node, generate inode if not present
    fn node_inode(&mut self, node: nx::Node<'a>) -> u64 {
        match self.entries.inode(node) {
            Some(inode) => inode,
            // Doesn't have an inode yet, generate one, and insert it to pairs
            None => {
                let inode = self.new_inode();
                self.entries.push(Entry { inode: inode, node: node,
                                          opt_data_inode: None });
                inode
            }
        }
    }
    fn node_file_attr(&mut self, node: nx::Node<'a>) -> FileAttr {
        let size = with_node_data(node, |d| d.len());
        FileAttr {
            ino: self.node_inode(node),
            size: size as u64,
            blocks: 1,
            atime: self.create_time,
            mtime: self.create_time,
            ctime: self.create_time,
            crtime: self.create_time,
            kind: node_file_type(node),
            perm: 0o644,
            nlink: 1,
            uid: 501,
            gid: 20,
            rdev: 0,
            flags: 0,
        }
    }
}

fn node_has_children(node: nx::Node) -> bool {
    node.iter().count() > 0
}

const TTL: Timespec = Timespec { sec: 1, nsec: 0 };

fn node_file_type(node: nx::Node) -> FileType {
    match node_has_children(node) {
        true => FileType::Directory,
        false => FileType::RegularFile,
    }
}

impl<'a> Filesystem for NxFilesystem<'a> {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &Path, reply: ReplyEntry) {
        let mut node = self.entries.node(parent).expect("[lookup] Invalid parent");
        for c in name.components() {
            if let Component::Normal(name) = c {
                let name = name.to_str().expect("Path component not valid utf-8");
                node = match node.get(name) {
                    Some(node) => node,
                    None => {
                        debugln!("[lookup] Couldn't find node with name \"{}\"", name);
                        reply.error(ENOENT);
                        return;
                    }
                }
            } else {
                panic!("[lookup] Invalid path component, only expected Normal.");
            }
        }
        reply.entry(&TTL, &self.node_file_attr(node), 0);
    }
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        let node = self.entries
                       .node(ino)
                       .unwrap_or_else(|| panic!("[read] No node with inode {} exists.", ino));
        reply.attr(&TTL, &self.node_file_attr(node));
    }
    fn read(&mut self,
            _req: &Request,
            ino: u64,
            _fh: u64,
            offset: u64,
            size: u32,
            reply: ReplyData) {
        debugln!("[read] ino: {}, offset: {}, size: {}", ino, offset, size);
        let node = self.entries
                       .node(ino)
                       .unwrap_or_else(|| panic!("[read] No node with inode {} exists.", ino));
        with_node_data(node,
                       |data| {
                           let from = offset as usize;
                           let to = ::std::cmp::min(from + size as usize, data.len());
                           debugln!("from {}, to {}, data.len {}", from, to, data.len());
                           reply.data(&data[from..to]);
                       });
    }
    fn readdir(&mut self,
               _req: &Request,
               ino: u64,
               _fh: u64,
               offset: u64,
               mut reply: ReplyDirectory) {
        debugln!("[readdir] ino: {}, offset: {}", ino, offset);
        if offset == 0 {
            let node_to_read = self.entries
                                   .node(ino)
                                   .expect("Trying to read nonexistent dir");
            for (i, child) in node_to_read.iter().enumerate() {
                let file_type = node_file_type(child);
                let inode = self.node_inode(child);
                reply.add(inode, (i + 1) as u64, file_type, child.name());
            }
        }
        reply.ok();
    }
}
