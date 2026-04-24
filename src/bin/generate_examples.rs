use std::fs::{self, File};
use std::io::{self, Cursor};
use std::path::Path;

use tar::{Builder, EntryType, Header};

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let examples_dir = Path::new(MANIFEST_DIR).join("examples");
    fs::create_dir_all(&examples_dir)?;
    let left_archive = examples_dir.join("left.tar");
    let right_archive = examples_dir.join("right.tar");

    write_archive(&left_archive, &build_left_entries())?;
    write_archive(&right_archive, &build_right_entries())?;

    println!("Wrote {}", left_archive.display());
    println!("Wrote {}", right_archive.display());
    Ok(())
}

#[derive(Clone, Copy)]
struct Metadata {
    mode: u32,
    uid: u64,
    gid: u64,
    uname: &'static str,
    gname: &'static str,
    mtime: u64,
}

struct RegularFile {
    path: &'static str,
    content: &'static [u8],
    metadata: Metadata,
}

struct Directory {
    path: &'static str,
    metadata: Metadata,
}

struct Link {
    path: &'static str,
    target: &'static str,
    metadata: Metadata,
    entry_type: EntryType,
    mode: u32,
}

struct Device {
    path: &'static str,
    metadata: Metadata,
    mode: u32,
    major: u32,
    minor: u32,
}

struct PaxExtension {
    key: &'static str,
    value: &'static str,
}

enum ExampleEntry {
    Directory(Directory),
    RegularFile {
        file: RegularFile,
        pax_extensions: &'static [PaxExtension],
    },
    Link(Link),
    Device(Device),
}

fn build_left_entries() -> Vec<ExampleEntry> {
    vec![
        ExampleEntry::Directory(Directory {
            path: "demo",
            metadata: Metadata {
                mode: 0o755,
                uid: 1000,
                gid: 1000,
                uname: "leftuser",
                gname: "leftgroup",
                mtime: 1_700_000_000,
            },
        }),
        ExampleEntry::Directory(Directory {
            path: "demo/meta",
            metadata: Metadata {
                mode: 0o755,
                uid: 1000,
                gid: 1000,
                uname: "leftuser",
                gname: "leftgroup",
                mtime: 1_700_000_010,
            },
        }),
        ExampleEntry::RegularFile {
            file: RegularFile {
                path: "demo/unchanged.txt",
                content: b"same content\n",
                metadata: Metadata {
                    mode: 0o644,
                    uid: 1000,
                    gid: 1000,
                    uname: "leftuser",
                    gname: "leftgroup",
                    mtime: 1_700_000_020,
                },
            },
            pax_extensions: &[],
        },
        ExampleEntry::RegularFile {
            file: RegularFile {
                path: "demo/changed.txt",
                content: b"left side payload\n",
                metadata: Metadata {
                    mode: 0o640,
                    uid: 1000,
                    gid: 1000,
                    uname: "leftuser",
                    gname: "leftgroup",
                    mtime: 1_700_000_030,
                },
            },
            pax_extensions: &[],
        },
        ExampleEntry::RegularFile {
            file: RegularFile {
                path: "demo/pax.txt",
                content: b"same pax payload\n",
                metadata: Metadata {
                    mode: 0o644,
                    uid: 1000,
                    gid: 1000,
                    uname: "leftuser",
                    gname: "leftgroup",
                    mtime: 1_700_000_040,
                },
            },
            pax_extensions: &[
                PaxExtension {
                    key: "comment",
                    value: "left pax comment",
                },
                PaxExtension {
                    key: "SCHILY.xattr.user.demo",
                    value: "alpha",
                },
            ],
        },
        ExampleEntry::Link(Link {
            path: "demo/tool-link",
            target: "changed.txt",
            metadata: Metadata {
                mode: 0o777,
                uid: 1000,
                gid: 1000,
                uname: "leftuser",
                gname: "leftgroup",
                mtime: 1_700_000_050,
            },
            entry_type: EntryType::symlink(),
            mode: 0o777,
        }),
        ExampleEntry::Link(Link {
            path: "demo/tool-hardlink",
            target: "demo/changed.txt",
            metadata: Metadata {
                mode: 0o644,
                uid: 1000,
                gid: 1000,
                uname: "leftuser",
                gname: "leftgroup",
                mtime: 1_700_000_060,
            },
            entry_type: EntryType::hard_link(),
            mode: 0o644,
        }),
        ExampleEntry::Device(Device {
            path: "demo/tty-demo",
            metadata: Metadata {
                mode: 0o600,
                uid: 0,
                gid: 0,
                uname: "root",
                gname: "wheel",
                mtime: 1_700_000_070,
            },
            mode: 0o600,
            major: 4,
            minor: 1,
        }),
        ExampleEntry::RegularFile {
            file: RegularFile {
                path: "demo/removed-only.txt",
                content: b"present only on the left\n",
                metadata: Metadata {
                    mode: 0o600,
                    uid: 1000,
                    gid: 1000,
                    uname: "leftuser",
                    gname: "leftgroup",
                    mtime: 1_700_000_080,
                },
            },
            pax_extensions: &[],
        },
        ExampleEntry::RegularFile {
            file: RegularFile {
                path: "demo/type-swap",
                content: b"I am a file on the left\n",
                metadata: Metadata {
                    mode: 0o644,
                    uid: 1000,
                    gid: 1000,
                    uname: "leftuser",
                    gname: "leftgroup",
                    mtime: 1_700_000_090,
                },
            },
            pax_extensions: &[],
        },
    ]
}

fn build_right_entries() -> Vec<ExampleEntry> {
    vec![
        ExampleEntry::Directory(Directory {
            path: "demo",
            metadata: Metadata {
                mode: 0o755,
                uid: 1001,
                gid: 1001,
                uname: "rightuser",
                gname: "rightgroup",
                mtime: 1_700_000_000,
            },
        }),
        ExampleEntry::Directory(Directory {
            path: "demo/meta",
            metadata: Metadata {
                mode: 0o700,
                uid: 1001,
                gid: 1001,
                uname: "rightuser",
                gname: "rightgroup",
                mtime: 1_700_000_110,
            },
        }),
        ExampleEntry::RegularFile {
            file: RegularFile {
                path: "demo/unchanged.txt",
                content: b"same content\n",
                metadata: Metadata {
                    mode: 0o644,
                    uid: 1000,
                    gid: 1000,
                    uname: "leftuser",
                    gname: "leftgroup",
                    mtime: 1_700_000_020,
                },
            },
            pax_extensions: &[],
        },
        ExampleEntry::RegularFile {
            file: RegularFile {
                path: "demo/changed.txt",
                content: b"right side payload with more bytes\n",
                metadata: Metadata {
                    mode: 0o600,
                    uid: 2000,
                    gid: 3000,
                    uname: "rightuser",
                    gname: "rightgroup",
                    mtime: 1_700_000_130,
                },
            },
            pax_extensions: &[],
        },
        ExampleEntry::RegularFile {
            file: RegularFile {
                path: "demo/pax.txt",
                content: b"same pax payload\n",
                metadata: Metadata {
                    mode: 0o644,
                    uid: 1000,
                    gid: 1000,
                    uname: "leftuser",
                    gname: "leftgroup",
                    mtime: 1_700_000_040,
                },
            },
            pax_extensions: &[
                PaxExtension {
                    key: "comment",
                    value: "right pax comment",
                },
                PaxExtension {
                    key: "SCHILY.xattr.user.demo",
                    value: "beta",
                },
            ],
        },
        ExampleEntry::Link(Link {
            path: "demo/tool-link",
            target: "pax.txt",
            metadata: Metadata {
                mode: 0o777,
                uid: 1001,
                gid: 1001,
                uname: "rightuser",
                gname: "rightgroup",
                mtime: 1_700_000_150,
            },
            entry_type: EntryType::symlink(),
            mode: 0o777,
        }),
        ExampleEntry::Link(Link {
            path: "demo/tool-hardlink",
            target: "demo/pax.txt",
            metadata: Metadata {
                mode: 0o644,
                uid: 1001,
                gid: 1001,
                uname: "rightuser",
                gname: "rightgroup",
                mtime: 1_700_000_160,
            },
            entry_type: EntryType::hard_link(),
            mode: 0o644,
        }),
        ExampleEntry::Device(Device {
            path: "demo/tty-demo",
            metadata: Metadata {
                mode: 0o660,
                uid: 0,
                gid: 0,
                uname: "root",
                gname: "wheel",
                mtime: 1_700_000_170,
            },
            mode: 0o660,
            major: 5,
            minor: 2,
        }),
        ExampleEntry::RegularFile {
            file: RegularFile {
                path: "demo/added-only.txt",
                content: b"present only on the right\n",
                metadata: Metadata {
                    mode: 0o644,
                    uid: 1001,
                    gid: 1001,
                    uname: "rightuser",
                    gname: "rightgroup",
                    mtime: 1_700_000_180,
                },
            },
            pax_extensions: &[],
        },
        ExampleEntry::Directory(Directory {
            path: "demo/type-swap",
            metadata: Metadata {
                mode: 0o755,
                uid: 1001,
                gid: 1001,
                uname: "rightuser",
                gname: "rightgroup",
                mtime: 1_700_000_190,
            },
        }),
    ]
}

fn write_archive(path: &Path, entries: &[ExampleEntry]) -> io::Result<()> {
    let file = File::create(path)?;
    let mut builder = Builder::new(file);

    for entry in entries {
        match entry {
            ExampleEntry::Directory(dir) => append_directory(&mut builder, dir)?,
            ExampleEntry::RegularFile {
                file,
                pax_extensions,
            } => append_regular_file(&mut builder, file, pax_extensions)?,
            ExampleEntry::Link(link) => append_link(&mut builder, link)?,
            ExampleEntry::Device(device) => append_device(&mut builder, device)?,
        }
    }

    builder.finish()?;
    Ok(())
}

fn append_directory(builder: &mut Builder<File>, directory: &Directory) -> io::Result<()> {
    let mut header = Header::new_ustar();
    apply_metadata(&mut header, directory.metadata)?;
    header.set_entry_type(EntryType::dir());
    header.set_mode(directory.metadata.mode);
    header.set_size(0);
    header.set_path(format!("{}/", directory.path.trim_end_matches('/')))?;
    header.set_cksum();
    builder.append(&header, io::empty())
}

fn append_regular_file(
    builder: &mut Builder<File>,
    file: &RegularFile,
    pax_extensions: &[PaxExtension],
) -> io::Result<()> {
    if !pax_extensions.is_empty() {
        builder.append_pax_extensions(
            pax_extensions
                .iter()
                .map(|extension| (extension.key, extension.value.as_bytes())),
        )?;
    }

    let mut header = Header::new_ustar();
    apply_metadata(&mut header, file.metadata)?;
    header.set_entry_type(EntryType::file());
    header.set_mode(file.metadata.mode);
    header.set_size(file.content.len() as u64);
    header.set_path(file.path)?;
    header.set_cksum();
    builder.append(&header, Cursor::new(file.content))
}

fn append_link(builder: &mut Builder<File>, link: &Link) -> io::Result<()> {
    let mut header = Header::new_ustar();
    apply_metadata(&mut header, link.metadata)?;
    header.set_entry_type(link.entry_type);
    header.set_mode(link.mode);
    header.set_size(0);
    header.set_path(link.path)?;
    header.set_link_name(link.target)?;
    header.set_cksum();
    builder.append(&header, io::empty())
}

fn append_device(builder: &mut Builder<File>, device: &Device) -> io::Result<()> {
    let mut header = Header::new_ustar();
    apply_metadata(&mut header, device.metadata)?;
    header.set_entry_type(EntryType::character_special());
    header.set_mode(device.mode);
    header.set_size(0);
    header.set_path(device.path)?;
    header.set_device_major(device.major)?;
    header.set_device_minor(device.minor)?;
    header.set_cksum();
    builder.append(&header, io::empty())
}

fn apply_metadata(header: &mut Header, metadata: Metadata) -> io::Result<()> {
    header.set_uid(metadata.uid);
    header.set_gid(metadata.gid);
    header.set_mtime(metadata.mtime);
    header.set_username(metadata.uname)?;
    header.set_groupname(metadata.gname)?;
    Ok(())
}
