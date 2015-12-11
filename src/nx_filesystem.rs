use fuse::{Filesystem, Request, ReplyEntry, ReplyDirectory, ReplyData, ReplyAttr, FileType,
           FileAttr, FUSE_ROOT_ID};
use std::path::{Path, Component};
use libc::ENOENT;
use nx::{self, GenericNode};
use time::{self, Timespec};
use std::collections::HashMap;
use byteorder::{LittleEndian, BigEndian, WriteBytesExt};

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
            entry.inodes.main == inode ||
            match entry.inodes.opt_data {
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
            use std::io::Write;
            let bitmap = node.bitmap().unwrap();
            let len = bitmap.len();
            let mut buf = vec![0; len as usize];
            bitmap.data(&mut buf);
            let header_size = 108;
            let offset = 2 + (3 * 4) + header_size;
            let size_bytes = offset + len;
            let mut bmp_data = Vec::<u8>::with_capacity(size_bytes as usize);
            // Write bmp header
            bmp_data.write(&[0x42, 0x4D]).unwrap();
            bmp_data.write_u32::<LittleEndian>(size_bytes).unwrap();
            // Zero out the reserved section
            bmp_data.write_u32::<LittleEndian>(0).unwrap();
            bmp_data.write_u32::<LittleEndian>(offset).unwrap();
            // Write bitmapinfoheader
            bmp_data.write_u32::<LittleEndian>(header_size).unwrap();
            bmp_data.write_i32::<LittleEndian>(bitmap.width() as i32).unwrap();
            bmp_data.write_i32::<LittleEndian>(-(bitmap.height() as i32)).unwrap();
            bmp_data.write_u16::<LittleEndian>(1).unwrap();
            bmp_data.write_u16::<LittleEndian>(32).unwrap();
            bmp_data.write_u32::<LittleEndian>(3).unwrap();
            bmp_data.write_u32::<LittleEndian>(len).unwrap();
            bmp_data.write_i32::<LittleEndian>(2835).unwrap();
            bmp_data.write_i32::<LittleEndian>(2835).unwrap();
            bmp_data.write_i32::<LittleEndian>(0).unwrap();
            bmp_data.write_i32::<LittleEndian>(0).unwrap();
            bmp_data.write_u32::<BigEndian>(0x0000FF00).unwrap(); // R
            bmp_data.write_u32::<BigEndian>(0x00FF0000).unwrap(); // G
            bmp_data.write_u32::<BigEndian>(0xFF000000).unwrap(); // B
            bmp_data.write_u32::<BigEndian>(0x000000FF).unwrap(); // A
            bmp_data.write(&[0x20, 0x6E, 0x69, 0x57]).unwrap();
            bmp_data.write(&[0; 0x24 + (3 * 4)]).unwrap();
            bmp_data.write(&buf).unwrap();
            func(&bmp_data)
        }
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
                let opt_data_inode = if node_has_children(node) &&
                                        node.dtype() != nx::Type::Empty {
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
        use std::collections::hash_map::Entry::*;
        let inodes = self.node_inodes(node);
        let mut size = None;
        let main_attr = match self.inode_attrs.entry(inodes.main) {
            Occupied(en) => *en.get(),
            Vacant(en) => {
                let s = with_node_data(node, |d| d.len()) as u64;
                size = Some(s);
                *en.insert(FileAttr {
                    ino: inodes.main,
                    size: s,
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
                })
            }
        };
        let opt_data_attr = if let Some(inode) = inodes.opt_data {
            let attr = match self.inode_attrs.entry(inode) {
                Occupied(en) => *en.get(),
                Vacant(en) => {
                    *en.insert(FileAttr {
                        ino: inode,
                        size: size.unwrap_or_else(|| with_node_data(node, |d| d.len()) as u64),
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
                    })
                }
            };
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
    if node_has_children(node) {
        FileType::Directory
    } else {
        FileType::RegularFile
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
                                        reply.error(ENOENT);
                                        return;
                                    }
                                }
                            }
                            None => {
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
        reply.entry(&TTL,
                    &(if data_request {
                        attrs.opt_data.unwrap()
                    } else {
                        attrs.main
                    }),
                    0);
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
        let node = self.entries
                       .nxnode(ino)
                       .unwrap_or_else(|| panic!("[read] No node with inode {} exists.", ino));
        with_node_data(node, |data| {
            let from = offset as usize;
            let to = ::std::cmp::min(from + size as usize, data.len());
            // Don't crash if from > to.
            // Don't know why this can occur though.
            let from = ::std::cmp::min(from, to);
            reply.data(&data[from..to]);
        });
    }
    fn readdir(&mut self,
               _req: &Request,
               ino: u64,
               _fh: u64,
               offset: u64,
               mut reply: ReplyDirectory) {
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
                    reply.add(inode,
                              (i + 2) as u64,
                              FileType::RegularFile,
                              &[child.name(), "_data"].concat());
                }
            }
        }
        reply.ok();
    }
}
