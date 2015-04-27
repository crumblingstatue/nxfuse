use fuse::{
    Filesystem,
    Request,
    ReplyEntry,
    ReplyDirectory,
    FileType,
    FileAttr,
    FUSE_ROOT_ID
};
use std::path::{Path, Component};
use libc::ENOENT;
use nx::{self, GenericNode};
use time::{self, Timespec};

pub struct NxFilesystem<'a> {
    nx_file: &'a nx::File,
    inode_node_pairs: InodeNodePairVec<'a>,
    /// Used to assign a new unique inode to nx nodes that don't have one yet
    inode_counter: u64,
    create_time: Timespec
}

struct InodeNodePair<'a> {
    inode: u64,
    node: nx::Node<'a>
}

struct InodeNodePairVec<'a> {
    vec: Vec<InodeNodePair<'a>>
}

impl<'a> InodeNodePairVec<'a> {
    fn new() -> InodeNodePairVec<'a> {
        InodeNodePairVec {
            vec: Vec::new()
        }
    }
    fn push(&mut self, pair: InodeNodePair<'a>) {
        self.vec.push(pair);
    }
    fn node(&self, inode: u64) -> Option<nx::Node<'a>> {
        match self.vec.iter().find(|p| p.inode == inode) {
            Some(pair) => Some(pair.node),
            None => None
        }
    }
    fn inode(&self, node: nx::Node) -> Option<u64> {
        match self.vec.iter().find(|p| p.node == node) {
            Some(pair) => Some(pair.inode),
            None => None
        }
    }
}

impl<'a> NxFilesystem<'a> {
    pub fn new_with_nx_file(nx_file: &'a nx::File) -> NxFilesystem {
        let pairs = InodeNodePairVec::new();
        let mut fs = NxFilesystem {
            nx_file: nx_file,
            inode_node_pairs: pairs,
            inode_counter: FUSE_ROOT_ID + 1,
            create_time: time::get_time()
        };
        // Add root node
        fs.inode_node_pairs.push(InodeNodePair{ inode: FUSE_ROOT_ID, node: fs.nx_file.root() });
        fs
    }
    fn new_inode(&mut self) -> u64 {
        let inode = self.inode_counter;
        self.inode_counter += 1;
        inode
    }
    /// Get inode for a node, generate inode if not present
    fn node_inode(&mut self, node: nx::Node<'a>) -> u64 {
        match self.inode_node_pairs.inode(node) {
            Some(inode) => inode,
            // Doesn't have an inode yet, generate one, and insert it to pairs
            None => {
                let inode = self.new_inode();
                self.inode_node_pairs.push(InodeNodePair{ inode: inode, node: node });
                inode
            }
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
        false => FileType::RegularFile
    }
}

impl<'a> Filesystem for NxFilesystem<'a> {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &Path, reply: ReplyEntry) {
        let mut node = self.inode_node_pairs.node(parent).expect("[lookup] Invalid parent");
        for c in name.components() {
            if let Component::Normal(name) = c {
                let name = name.to_str().expect("Path component not valid utf-8");
                node = match node.get(name) {
                    Some(node) => node,
                    None => {
                        println!("[lookup] Couldn't find node with name \"{}\"", name);
                        reply.error(ENOENT);
                        return;
                    }
                }
            } else {
                panic!("[lookup] Invalid path component, only expected Normal.");
            }
        }
        let size = 0;
        let attr = FileAttr {
            ino: self.node_inode(node),
            size: size,
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
            flags: 0
        };
        reply.entry(&TTL, &attr, 0);
    }
    fn readdir(&mut self, _req: &Request, _ino: u64, _fh: u64, offset: u64,
               mut reply: ReplyDirectory) {
        println!("[readdir] ino: {}, offset: {}", offset, _ino);
        // Ignore inode 0
        if _ino == 0 {
            reply.error(ENOENT);
            return;
        }
        // For some reason we assert here that we are at offset 0
        if offset == 0 {
            let node_to_read = self.inode_node_pairs.node(_ino)
                               .expect("Trying to read nonexistent dir");
            for (i, child) in node_to_read.iter().enumerate() {
                let file_type = node_file_type(child);
                let inode = self.node_inode(child);
                reply.add(inode, i as u64, file_type, child.name());
            }
        }
        reply.ok();
    }
}
