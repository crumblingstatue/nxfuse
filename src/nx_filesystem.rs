use fuse::{Filesystem, Request, ReplyEntry, ReplyDirectory, ReplyData, ReplyAttr, FileType,
           FileAttr, FUSE_ROOT_ID};
use std::path::{Path, Component};
use libc::ENOENT;
use nx::{self, GenericNode};
use time::{self, Timespec};
use std::collections::HashMap;
use image::png::PNGEncoder;
use image::ColorType;

pub struct NxFilesystem<'a> {
    nx_file: &'a nx::File,
    entries: Entries<'a>,
    /// Used to assign a new unique inode to nx nodes that don't have one yet
    inode_counter: u64,
    create_time: Timespec,
    inode_attrs: HashMap<u64, FileAttr>,
}

struct Entry<'a> {
    inodes: NodeInodes,
    nxnode: nx::Node<'a>,
}

struct Entries<'a> {
    vec: Vec<Entry<'a>>,
}

/// Inodes of an nx::Node
#[derive(Clone, Copy)]
struct NodeInodes {
    /// The main inode.
    ///
    /// This is either a regular file, or a directory, if the node has children.
    main: u64,
    /// Optional data inode for entries that have both data, and children.
    ///
    /// In such cases there is a folder with the name of the node (e.g. `foo`), and
    /// a file with the data and _data appended (e.g. `foo_data`).
    ///
    /// This field represents the inode for that optional "foo_data" file.
    opt_data: Option<u64>,
}

/// File attributes of an nx::Node
struct NodeFileAttrs {
    main: FileAttr,
    opt_data: Option<FileAttr>,
}

impl<'a> Entries<'a> {
    fn new() -> Self {
        Entries { vec: Vec::new() }
    }
    fn push(&mut self, pair: Entry<'a>) {
        self.vec.push(pair);
    }
    /// The nx node belonging to an inode
    fn nxnode(&self, inode: u64) -> Option<nx::Node<'a>> {
        match self.vec.iter().find(|entry| {
            entry.inodes.main == inode || match entry.inodes.opt_data {
                Some(ino) => inode == ino,
                None => false,
            }
        }) {
            Some(entry) => Some(entry.nxnode),
            None => None,
        }
    }
    fn inodes(&self, node: nx::Node) -> Option<NodeInodes> {
        match self.vec.iter().find(|entry| entry.nxnode == node) {
            Some(entry) => Some(entry.inodes),
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
        nx::Type::Bitmap => {
            let bitmap = node.bitmap().unwrap();
            let mut buf = vec![0; bitmap.len() as usize];
            bitmap.data(&mut buf);
            let mut png_data = Vec::<u8>::new();
            {
                let enc = PNGEncoder::new(&mut png_data);
                enc.encode(&buf, bitmap.width() as u32, bitmap.height() as u32, ColorType::RGBA(8)).unwrap();
            }
            func(&png_data)
        },
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
            inode_attrs: HashMap::new(),
        };
        // Add root node
        let attr = FileAttr {
            ino: FUSE_ROOT_ID,
            size: 0,
            blocks: 1,
            atime: fs.create_time,
            mtime: fs.create_time,
            ctime: fs.create_time,
            crtime: fs.create_time,
            kind: FileType::Directory,
            perm: 0o644,
            nlink: 1,
            uid: 501,
            gid: 20,
            rdev: 0,
            flags: 0,
        };
        fs.inode_attrs.insert(FUSE_ROOT_ID, attr);
        fs.entries.push(Entry {
            nxnode: fs.nx_file.root(),
            inodes: NodeInodes {
                main: FUSE_ROOT_ID,
                opt_data: None,
            },
        });
        fs
    }
    fn new_inode(&mut self) -> u64 {
        let inode = self.inode_counter;
        self.inode_counter += 1;
        inode
    }
    /// Get inodes for a node, generate them if not present
    fn node_inodes(&mut self, node: nx::Node<'a>) -> NodeInodes {
        match self.entries.inodes(node) {
            Some(inodes) => inodes,
            // Doesn't have inodes yet, generate and insert them
            None => {
                let main_inode = self.new_inode();
                let opt_data_inode = if node_has_children(node) && node.dtype() != nx::Type::Empty {
                    Some(self.new_inode())
                } else {
                    None
                };
                let inodes = NodeInodes {
                    main: main_inode,
                    opt_data: opt_data_inode,
                };
                self.entries.push(Entry {
                    nxnode: node,
                    inodes: inodes,
                });
                inodes
            }
        }
    }
    fn node_file_attrs(&mut self, node: nx::Node<'a>) -> NodeFileAttrs {
        let inodes = self.node_inodes(node);
        let size = with_node_data(node, |d| d.len());
        let main_attr = FileAttr {
            ino: inodes.main,
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
        };
        self.inode_attrs.insert(inodes.main, main_attr);
        let opt_data_attr = if let Some(inode) = inodes.opt_data {
            let attr = FileAttr {
                ino: inode,
                size: size as u64,
                blocks: 1,
                atime: self.create_time,
                mtime: self.create_time,
                ctime: self.create_time,
                crtime: self.create_time,
                kind: FileType::RegularFile,
                perm: 0o644,
                nlink: 1,
                uid: 501,
                gid: 20,
                rdev: 0,
                flags: 0,
            };
            self.inode_attrs.insert(inode, attr);
            Some(attr)
        } else {
            None
        };
        NodeFileAttrs {
            main: main_attr,
            opt_data: opt_data_attr,
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
        let mut node = self.entries.nxnode(parent).expect("[lookup] Invalid parent");
        let mut data_request = false;
        for c in name.components() {
            if let Component::Normal(name) = c {
                let name = name.to_str().expect("Path component not valid utf-8");
                node = match node.get(name) {
                    Some(node) => node,
                    None => {
                        // See if the name ends with _data, in which case it's a data request
                        match name.rfind("_data") {
                            Some(pos) => {
                                data_request = true;
                                match node.get(&name[..pos]) {
                                    Some(node) => node,
                                    None => {
                                        debugln!("[lookup] Couldn't find node with name \"{}\"", name);
                                        reply.error(ENOENT);
                                        return;
                                    }
                                }
                            }
                            None => {
                                debugln!("[lookup] Couldn't find node with name \"{}\"", name);
                                reply.error(ENOENT);
                                return;
                            }
                        }
                    }
                }
            } else {
                panic!("[lookup] Invalid path component, only expected Normal.");
            }
        }
        let attrs = self.node_file_attrs(node);
        reply.entry(&TTL, &(if data_request { attrs.opt_data.unwrap() } else { attrs.main }), 0);
    }
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        match self.inode_attrs.get(&ino) {
            Some(attr) => reply.attr(&TTL, attr),
            None => panic!("Could not get attribute for inode {}", ino),
        }
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
                       .nxnode(ino)
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
                                   .nxnode(ino)
                                   .expect("Trying to read nonexistent dir");
            for (i, child) in node_to_read.iter().enumerate() {
                let file_type = node_file_type(child);
                let inodes = self.node_inodes(child);
                // Generate the attributes for the node
                self.node_file_attrs(child);
                reply.add(inodes.main, (i + 1) as u64, file_type, child.name());
                if let Some(inode) = inodes.opt_data {
                    reply.add(inode, (i + 2) as u64, FileType::RegularFile, &[child.name(), "_data"].concat());
                }
            }
        }
        reply.ok();
    }
}
