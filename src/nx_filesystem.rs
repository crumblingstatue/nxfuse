use fuse::{
    Filesystem,
    Request,
    ReplyDirectory,
    FileType,
    FUSE_ROOT_ID
};
use libc::ENOENT;
use nx;

pub struct NxFilesystem<'a> {
    nx_file: &'a nx::File,
    inode_node_pairs: InodeNodePairVec<'a>,
    /// Used to assign a new unique inode to nx nodes that don't have one yet
    inode_counter: u64
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
            inode_counter: FUSE_ROOT_ID + 1
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
}

fn node_has_children(node: nx::Node) -> bool {
    node.iter().count() > 0
}

impl<'a> Filesystem for NxFilesystem<'a> {
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
                let file_type = match node_has_children(child) {
                    true => FileType::Directory,
                    false => FileType::RegularFile
                };
                let inode = match self.inode_node_pairs.inode(child) {
                    Some(inode) => inode,
                    // Doesn't have an inode yet, generate one, and insert it to pairs
                    None => {
                        let inode = self.new_inode();
                        self.inode_node_pairs.push(InodeNodePair{ inode: inode, node: child });
                        inode
                    }
                };
                reply.add(inode, i as u64, file_type, child.name());
            }
        }
        reply.ok();
    }
}
