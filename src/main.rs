use bzip2::read::BzDecoder;
use clap::Parser;
use comfy_table::modifiers::UTF8_SOLID_INNER_BORDERS;
use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table, presets::UTF8_FULL};
use console::style;
use flate2::read::GzDecoder;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::ExitCode,
    thread,
};
use strum::Display;
use tar::{Archive, EntryType};
use thiserror::Error;
use time::{OffsetDateTime, macros::format_description};
use xz2::read::XzDecoder;
use zstd::stream::read::Decoder as ZstdDecoder;

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool, AppError> {
    let args = Cli::parse();
    let left_path = args.left;
    let right_path = args.right;
    let progress = (!args.quiet).then(MultiProgress::new);
    let render_mode = match (args.table, args.list) {
        (false, true) => RenderMode::List,
        _ => RenderMode::Table,
    };
    let compact = match (args.compact, args.spacious, render_mode) {
        (true, false, _) => true,
        (false, true, _) => false,
        (false, false, RenderMode::Table) => true,
        (false, false, RenderMode::List) => false,
        (true, true, _) => unreachable!("clap enforces conflicts"),
    };

    let (left_entries, right_entries) = thread::scope(|scope| {
        let left_handle = scope.spawn(|| load_archive(&left_path, progress.as_ref()));
        let right_handle = scope.spawn(|| load_archive(&right_path, progress.as_ref()));

        let left_entries = left_handle
            .join()
            .map_err(|_| AppError::WorkerPanic("left archive worker"))??;
        let right_entries = right_handle
            .join()
            .map_err(|_| AppError::WorkerPanic("right archive worker"))??;

        Ok::<_, AppError>((left_entries, right_entries))
    })?;

    let rows = collect_diff_rows(&left_entries, &right_entries);
    let archives_equal = rows.is_empty();

    if !args.quiet {
        print_diff(&rows, render_mode, compact);
    }

    Ok(archives_equal)
}

#[derive(Debug, Parser)]
#[command(name = "tardiff")]
#[command(about = "Compare two tar archives by entry name, size, and checksum")]
struct Cli {
    /// Suppress progress and diff output. Exit status still indicates equality.
    #[arg(short, long)]
    quiet: bool,
    /// Use the compact rendering for the selected output mode.
    #[arg(short = 'c', long, conflicts_with = "spacious")]
    compact: bool,
    /// Use the spacious rendering for the selected output mode.
    #[arg(short = 's', long, conflicts_with = "compact")]
    spacious: bool,
    /// Render differences as a table.
    #[arg(short = 't', long, conflicts_with = "list")]
    table: bool,
    /// Render differences as a single unaligned line per entry.
    #[arg(short = 'l', long)]
    list: bool,
    /// Left-hand archive path.
    left: PathBuf,
    /// Right-hand archive path.
    right: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RenderMode {
    Table,
    List,
}

#[derive(Debug, Error)]
enum AppError {
    #[error("Failed to read {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("Failed to parse tar archive {path}: {source}")]
    Tar { path: PathBuf, source: io::Error },
    #[error("Archive {archive} contains an entry path that is not valid UTF-8")]
    UnsupportedEntryPath { archive: PathBuf },
    #[error("{0} panicked while loading an archive")]
    WorkerPanic(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EntryInfo {
    // Store rendered metadata so both output modes compare and print the exact same values.
    kind: EntryKind,
    size: String,
    checksum: Option<String>,
    mode: String,
    uid: String,
    gid: String,
    uname: String,
    gname: String,
    mtime: String,
    link_target: String,
    device_major: String,
    device_minor: String,
    pax_extensions: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display)]
enum EntryKind {
    #[strum(serialize = "file")]
    File,
    #[strum(serialize = "dir")]
    Directory,
    #[strum(serialize = "symlink")]
    Symlink,
    #[strum(serialize = "hardlink")]
    HardLink,
    #[strum(serialize = "other")]
    Other,
}

impl From<EntryType> for EntryKind {
    fn from(value: EntryType) -> Self {
        if value.is_file() {
            Self::File
        } else if value.is_dir() {
            Self::Directory
        } else if value.is_symlink() {
            Self::Symlink
        } else if value.is_hard_link() {
            Self::HardLink
        } else {
            Self::Other
        }
    }
}

fn load_archive(
    path: &Path,
    progress: Option<&MultiProgress>,
) -> Result<BTreeMap<String, EntryInfo>, AppError> {
    let file = File::open(path).map_err(|source| AppError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let progress_bar = progress
        .map(|progress| progress_bar_for(path, &file, progress))
        .transpose()?;
    let archive_reader = match &progress_bar {
        Some(progress_bar) => open_archive_reader(progress_bar.wrap_read(file), path)?,
        None => open_archive_reader(file, path)?,
    };
    let mut archive = Archive::new(archive_reader);
    let mut entries = BTreeMap::new();

    let iter = archive.entries().map_err(|source| AppError::Tar {
        path: path.to_path_buf(),
        source,
    })?;

    for entry_result in iter {
        let mut entry = entry_result.map_err(|source| AppError::Tar {
            path: path.to_path_buf(),
            source,
        })?;
        let entry_path = entry
            .path()
            .map_err(|source| AppError::Tar {
                path: path.to_path_buf(),
                source,
            })?
            .to_string_lossy()
            .into_owned();

        if entry_path.contains('\u{fffd}') {
            return Err(AppError::UnsupportedEntryPath {
                archive: path.to_path_buf(),
            });
        }

        let entry_type = entry.header().entry_type();
        let kind = EntryKind::from(entry_type);
        let pax_extensions = collect_pax_extensions(&mut entry, path)?;
        let size = entry.size().to_string();
        let checksum = if kind == EntryKind::File {
            Some(hash_reader(&mut entry).map_err(|source| AppError::Io {
                path: path.to_path_buf(),
                source,
            })?)
        } else {
            None
        };
        let mode = format_mode(
            entry_type,
            entry.header().mode().map_err(|source| AppError::Tar {
                path: path.to_path_buf(),
                source,
            })?,
        );
        let uid = pax_extensions
            .get("uid")
            .cloned()
            .unwrap_or_else(|| entry.header().uid().unwrap_or_default().to_string());
        let gid = pax_extensions
            .get("gid")
            .cloned()
            .unwrap_or_else(|| entry.header().gid().unwrap_or_default().to_string());
        let uname = pax_extensions
            .get("uname")
            .cloned()
            .or_else(|| bytes_to_string(entry.header().username_bytes()))
            .unwrap_or_else(missing_value);
        let gname = pax_extensions
            .get("gname")
            .cloned()
            .or_else(|| bytes_to_string(entry.header().groupname_bytes()))
            .unwrap_or_else(missing_value);
        let mtime = format_mtime(
            pax_extensions.get("mtime").map(String::as_str),
            entry.header().mtime().ok(),
        );
        let link_target =
            bytes_to_string(entry.link_name_bytes().as_deref()).unwrap_or_else(missing_value);
        let device_major = entry
            .header()
            .device_major()
            .ok()
            .flatten()
            .map(|value| value.to_string())
            .unwrap_or_else(missing_value);
        let device_minor = entry
            .header()
            .device_minor()
            .ok()
            .flatten()
            .map(|value| value.to_string())
            .unwrap_or_else(missing_value);
        let pax_extensions = format_pax_extensions(&pax_extensions);

        entries.insert(
            entry_path,
            EntryInfo {
                kind,
                size,
                checksum,
                mode,
                uid,
                gid,
                uname,
                gname,
                mtime,
                link_target,
                device_major,
                device_minor,
                pax_extensions,
            },
        );
    }

    if let Some(progress_bar) = progress_bar {
        progress_bar.finish_and_clear();
    }
    Ok(entries)
}

// Collect resolved PAX records for the current entry so diffing can show the
// effective metadata values rather than only the raw header fields.
fn collect_pax_extensions<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    path: &Path,
) -> Result<BTreeMap<String, String>, AppError> {
    let mut pairs = BTreeMap::new();

    if let Some(extensions) = entry.pax_extensions().map_err(|source| AppError::Tar {
        path: path.to_path_buf(),
        source,
    })? {
        for extension in extensions {
            let extension = extension.map_err(|source| AppError::Tar {
                path: path.to_path_buf(),
                source,
            })?;
            pairs.insert(
                String::from_utf8_lossy(extension.key_bytes()).into_owned(),
                String::from_utf8_lossy(extension.value_bytes()).into_owned(),
            );
        }
    }

    Ok(pairs)
}

fn bytes_to_string(bytes: Option<&[u8]>) -> Option<String> {
    bytes.map(|bytes| String::from_utf8_lossy(bytes).into_owned())
}

fn format_pax_extensions(extensions: &BTreeMap<String, String>) -> String {
    if extensions.is_empty() {
        missing_value()
    } else {
        extensions
            .iter()
            .map(|(key, value)| format!("  {key} = {value}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn missing_value() -> String {
    "-".to_string()
}

fn format_mode(entry_type: EntryType, mode: u32) -> String {
    let numeric = format!("{mode:#o}");
    let symbolic = format_mode_symbolic(entry_type, mode);
    format!("{numeric} ({symbolic})")
}

fn format_mode_symbolic(entry_type: EntryType, mode: u32) -> String {
    let mut rendered = String::with_capacity(10);
    rendered.push(match () {
        _ if entry_type.is_file() => '-',
        _ if entry_type.is_dir() => 'd',
        _ if entry_type.is_symlink() => 'l',
        _ if entry_type.is_hard_link() => 'h',
        _ if entry_type == EntryType::character_special() => 'c',
        _ if entry_type == EntryType::block_special() => 'b',
        _ if entry_type == EntryType::fifo() => 'p',
        _ => '?',
    });

    rendered.push(permission_char(mode, 0o400, 'r'));
    rendered.push(permission_char(mode, 0o200, 'w'));
    rendered.push(execute_char(mode, 0o100, 0o4000, 's', 'S', 'x'));
    rendered.push(permission_char(mode, 0o040, 'r'));
    rendered.push(permission_char(mode, 0o020, 'w'));
    rendered.push(execute_char(mode, 0o010, 0o2000, 's', 'S', 'x'));
    rendered.push(permission_char(mode, 0o004, 'r'));
    rendered.push(permission_char(mode, 0o002, 'w'));
    rendered.push(execute_char(mode, 0o001, 0o1000, 't', 'T', 'x'));

    rendered
}

fn permission_char(mode: u32, bit: u32, ch: char) -> char {
    if mode & bit != 0 { ch } else { '-' }
}

fn execute_char(
    mode: u32,
    exec_bit: u32,
    special_bit: u32,
    both: char,
    special: char,
    exec: char,
) -> char {
    match (mode & exec_bit != 0, mode & special_bit != 0) {
        (true, true) => both,
        (false, true) => special,
        (true, false) => exec,
        (false, false) => '-',
    }
}

fn format_mtime(pax_mtime: Option<&str>, header_mtime: Option<u64>) -> String {
    let raw = pax_mtime
        .map(str::to_owned)
        .or_else(|| header_mtime.map(|mtime| mtime.to_string()))
        .unwrap_or_else(missing_value);

    let seconds = pax_mtime
        .and_then(|mtime| mtime.parse::<f64>().ok())
        .map(|mtime| mtime.floor() as i64)
        .or_else(|| header_mtime.and_then(|mtime| i64::try_from(mtime).ok()));

    match seconds.and_then(format_unix_timestamp) {
        Some(human) => format!("{raw} ({human})"),
        None => raw,
    }
}

fn format_unix_timestamp(seconds: i64) -> Option<String> {
    let format = format_description!("[year]-[month]-[day] [hour]:[minute]:[second] UTC");
    OffsetDateTime::from_unix_timestamp(seconds)
        .ok()
        .and_then(|timestamp| timestamp.format(&format).ok())
}

fn hash_reader(reader: &mut impl Read) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8 * 1024];

    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    let digest = hasher.finalize();
    Ok(digest
        .as_slice()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn open_archive_reader(
    reader: impl Read + 'static,
    path: &Path,
) -> Result<Box<dyn Read>, AppError> {
    let mut base_reader = BufReader::new(reader);
    let signature = base_reader
        .fill_buf()
        .map_err(|source| AppError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .iter()
        .take(6)
        .copied()
        .collect::<Vec<_>>();

    let boxed: Box<dyn Read> = if signature.starts_with(&[0x1f, 0x8b]) {
        Box::new(GzDecoder::new(base_reader))
    } else if signature.starts_with(b"BZh") {
        Box::new(BzDecoder::new(base_reader))
    } else if signature.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0x00]) {
        Box::new(XzDecoder::new(base_reader))
    } else if signature.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        let decoder = ZstdDecoder::new(base_reader).map_err(|source| AppError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Box::new(decoder)
    } else {
        Box::new(base_reader)
    };

    Ok(boxed)
}

fn progress_bar_for(
    path: &Path,
    file: &File,
    progress: &MultiProgress,
) -> Result<ProgressBar, AppError> {
    let total_bytes = file
        .metadata()
        .map_err(|source| AppError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .len();
    let progress = progress.add(ProgressBar::new(total_bytes));
    let style = ProgressStyle::with_template(
        "{msg} [{wide_bar:.cyan/blue}] {bytes:>8}/{total_bytes:8} ({eta})",
    )
    .expect("progress template should be valid")
    .progress_chars("=> ");
    progress.set_style(style);
    progress.set_message(path.display().to_string());
    progress.set_position(0);
    Ok(progress)
}

fn print_diff(rows: &[DiffRow], render_mode: RenderMode, compact: bool) {
    if rows.is_empty() {
        println!(" No differences found.");
    } else {
        match (render_mode, compact) {
            (RenderMode::Table, true) => print_diff_table_compact(rows),
            (RenderMode::Table, false) => print_diff_table(rows),
            (RenderMode::List, true) => print_diff_lines(rows),
            (RenderMode::List, false) => print_diff_lines_expanded(rows),
        }
        println!();
        print_summary(rows);
    }
}

fn print_summary(rows: &[DiffRow]) {
    let added = rows
        .iter()
        .filter(|row| row.status == DiffStatus::Added)
        .count();
    let removed = rows
        .iter()
        .filter(|row| row.status == DiffStatus::Removed)
        .count();
    let changed = rows
        .iter()
        .filter(|row| row.status == DiffStatus::Changed)
        .count();

    println!(
        " {}. {}. {}.",
        style(format_count(changed, "changed")).yellow().bold(),
        style(format_count(added, "added")).green().bold(),
        style(format_count(removed, "removed")).red().bold(),
    );
}

fn format_count(count: usize, label: &str) -> String {
    format!("{count} {label}")
}

fn collect_diff_rows(
    left: &BTreeMap<String, EntryInfo>,
    right: &BTreeMap<String, EntryInfo>,
) -> Vec<DiffRow> {
    let mut all_paths = BTreeSet::new();
    all_paths.extend(left.keys().cloned());
    all_paths.extend(right.keys().cloned());

    let mut rows = Vec::new();

    for path in all_paths {
        match (left.get(&path), right.get(&path)) {
            (Some(left_info), Some(right_info)) if left_info == right_info => {}
            (None, Some(right_info)) => {
                rows.push(DiffRow::new(
                    DiffStatus::Added,
                    path,
                    None,
                    Some(right_info),
                ));
            }
            (Some(left_info), None) => {
                rows.push(DiffRow::new(
                    DiffStatus::Removed,
                    path,
                    Some(left_info),
                    None,
                ));
            }
            (Some(left_info), Some(right_info)) => {
                rows.push(DiffRow::new(
                    DiffStatus::Changed,
                    path,
                    Some(left_info),
                    Some(right_info),
                ));
            }
            (None, None) => {}
        }
    }

    rows
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiffStatus {
    Added,
    Removed,
    Changed,
}

impl DiffStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Added => "ADDED",
            Self::Removed => "REMOVED",
            Self::Changed => "CHANGED",
        }
    }

    fn list_symbol(self) -> &'static str {
        match self {
            Self::Added => "+",
            Self::Removed => "-",
            Self::Changed => "~",
        }
    }

    fn color(self) -> Color {
        match self {
            Self::Added => Color::Green,
            Self::Removed => Color::Red,
            Self::Changed => Color::Yellow,
        }
    }
}

#[derive(Debug)]
struct DiffRow {
    status: DiffStatus,
    path: String,
    left: Option<EntryInfo>,
    right: Option<EntryInfo>,
}

impl DiffRow {
    fn new(
        status: DiffStatus,
        path: String,
        left: Option<&EntryInfo>,
        right: Option<&EntryInfo>,
    ) -> Self {
        Self {
            status,
            path,
            left: left.cloned(),
            right: right.cloned(),
        }
    }

    fn attribute_rows(&self) -> Vec<AttributeDiff> {
        // Build the per-attribute diff view once and let each renderer decide
        // whether to show it as rows, blocks, or a compact joined string.
        let left = self.left.as_ref();
        let right = self.right.as_ref();
        let mut rows = Vec::new();

        add_attribute_row(
            &mut rows,
            "type",
            left.map(|entry| entry.kind.to_string()),
            right.map(|entry| entry.kind.to_string()),
            self.status,
        );
        add_attribute_row(
            &mut rows,
            "size",
            left.map(|entry| entry.size.clone()),
            right.map(|entry| entry.size.clone()),
            self.status,
        );
        add_attribute_row(
            &mut rows,
            "checksum",
            left.and_then(|entry| entry.checksum.clone()),
            right.and_then(|entry| entry.checksum.clone()),
            self.status,
        );
        add_attribute_row(
            &mut rows,
            "mode",
            left.map(|entry| entry.mode.clone()),
            right.map(|entry| entry.mode.clone()),
            self.status,
        );
        add_attribute_row(
            &mut rows,
            "uid",
            left.map(|entry| entry.uid.clone()),
            right.map(|entry| entry.uid.clone()),
            self.status,
        );
        add_attribute_row(
            &mut rows,
            "gid",
            left.map(|entry| entry.gid.clone()),
            right.map(|entry| entry.gid.clone()),
            self.status,
        );
        add_attribute_row(
            &mut rows,
            "uname",
            left.map(|entry| entry.uname.clone()),
            right.map(|entry| entry.uname.clone()),
            self.status,
        );
        add_attribute_row(
            &mut rows,
            "gname",
            left.map(|entry| entry.gname.clone()),
            right.map(|entry| entry.gname.clone()),
            self.status,
        );
        add_attribute_row(
            &mut rows,
            "mtime",
            left.map(|entry| entry.mtime.clone()),
            right.map(|entry| entry.mtime.clone()),
            self.status,
        );

        let left_link = left.and_then(|entry| match entry.kind {
            EntryKind::Symlink | EntryKind::HardLink => Some(entry.link_target.clone()),
            _ => None,
        });
        let right_link = right.and_then(|entry| match entry.kind {
            EntryKind::Symlink | EntryKind::HardLink => Some(entry.link_target.clone()),
            _ => None,
        });
        add_attribute_row(&mut rows, "link", left_link, right_link, self.status);

        let left_dev_major = left.and_then(|entry| {
            if entry.kind == EntryKind::Other && entry.device_major != "-" {
                Some(entry.device_major.clone())
            } else {
                None
            }
        });
        let right_dev_major = right.and_then(|entry| {
            if entry.kind == EntryKind::Other && entry.device_major != "-" {
                Some(entry.device_major.clone())
            } else {
                None
            }
        });
        add_attribute_row(
            &mut rows,
            "dev-major",
            left_dev_major,
            right_dev_major,
            self.status,
        );

        let left_dev_minor = left.and_then(|entry| {
            if entry.kind == EntryKind::Other && entry.device_minor != "-" {
                Some(entry.device_minor.clone())
            } else {
                None
            }
        });
        let right_dev_minor = right.and_then(|entry| {
            if entry.kind == EntryKind::Other && entry.device_minor != "-" {
                Some(entry.device_minor.clone())
            } else {
                None
            }
        });
        add_attribute_row(
            &mut rows,
            "dev-minor",
            left_dev_minor,
            right_dev_minor,
            self.status,
        );

        let left_pax = left.and_then(|entry| {
            if entry.pax_extensions != "-" {
                Some(entry.pax_extensions.clone())
            } else {
                None
            }
        });
        let right_pax = right.and_then(|entry| {
            if entry.pax_extensions != "-" {
                Some(entry.pax_extensions.clone())
            } else {
                None
            }
        });
        add_attribute_row(&mut rows, "pax", left_pax, right_pax, self.status);

        rows
    }
}

#[derive(Debug)]
struct AttributeDiff {
    name: &'static str,
    left: String,
    right: String,
}

impl AttributeDiff {
    fn left_display(&self) -> String {
        format_attribute_value(self.name, &self.left)
    }

    fn right_display(&self) -> String {
        format_attribute_value(self.name, &self.right)
    }
}

fn add_attribute_row(
    rows: &mut Vec<AttributeDiff>,
    name: &'static str,
    left: Option<String>,
    right: Option<String>,
    status: DiffStatus,
) {
    if left.is_none() && right.is_none() {
        return;
    }

    let left = left.unwrap_or_else(missing_value);
    let right = right.unwrap_or_else(missing_value);

    if status == DiffStatus::Changed && left == right {
        return;
    }

    rows.push(AttributeDiff { name, left, right });
}

fn print_diff_table(rows: &[DiffRow]) {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_SOLID_INNER_BORDERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            header_cell("Status"),
            header_cell("Path"),
            header_cell("Attribute"),
            header_cell("Left"),
            header_cell("Right"),
        ]);

    for row in rows {
        let attribute_rows = row.attribute_rows();
        for (index, attribute) in attribute_rows.iter().enumerate() {
            table.add_row(vec![
                if index == 0 {
                    status_cell(row.status)
                } else {
                    Cell::new("")
                },
                if index == 0 {
                    Cell::new(&row.path)
                } else {
                    Cell::new("")
                },
                Cell::new(attribute.name),
                value_cell(attribute.left_display()),
                value_cell(attribute.right_display()),
            ]);
        }
    }

    let rendered = table.to_string();
    for line in rendered.lines() {
        println!(" {line}");
    }
}

fn print_diff_table_compact(rows: &[DiffRow]) {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .apply_modifier(UTF8_SOLID_INNER_BORDERS)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            header_cell("Status"),
            header_cell("Path"),
            header_cell("Changes"),
        ]);

    for row in rows {
        let changes = compact_fields(row).join("\n");

        table.add_row(vec![
            status_cell(row.status),
            Cell::new(&row.path),
            Cell::new(changes),
        ]);
    }

    let rendered = table.to_string();
    for line in rendered.lines() {
        println!(" {line}");
    }
}

fn print_diff_lines(rows: &[DiffRow]) {
    for row in rows {
        println!(
            " {} {} | {}",
            colored_list_status(row.status),
            row.path,
            compact_fields(row).join(" | ")
        );
    }
}

fn print_diff_lines_expanded(rows: &[DiffRow]) {
    for row in rows {
        println!(
            " {} {}",
            colored_list_status(row.status),
            style(&row.path).bold()
        );
        for attribute in row.attribute_rows() {
            match row.status {
                DiffStatus::Added => {
                    println!("     {}: {}", attribute.name, attribute.right_display())
                }
                DiffStatus::Removed => {
                    println!("     {}: {}", attribute.name, attribute.left_display())
                }
                DiffStatus::Changed => {
                    println!(
                        "     {}: {} -> {}",
                        attribute.name,
                        attribute.left_display(),
                        attribute.right_display()
                    )
                }
            }
        }
    }
}

fn format_attribute_value(name: &str, value: &str) -> String {
    match name {
        "mode" | "mtime" => grey_parenthesized_suffix(value),
        _ => value.to_string(),
    }
}

fn grey_parenthesized_suffix(value: &str) -> String {
    match value.find(" (") {
        Some(index) if value.ends_with(')') => {
            format!("{}{}", &value[..index], style(&value[index..]).color256(8))
        }
        _ => value.to_string(),
    }
}

fn compact_fields(row: &DiffRow) -> Vec<String> {
    row.attribute_rows()
        .into_iter()
        .map(|attribute| match row.status {
            DiffStatus::Added => format!("{} = {}", attribute.name, attribute.right_display()),
            DiffStatus::Removed => format!("{} = {}", attribute.name, attribute.left_display()),
            DiffStatus::Changed => format!(
                "{}: {} -> {}",
                attribute.name,
                attribute.left_display(),
                attribute.right_display()
            ),
        })
        .collect()
}

fn header_cell(label: &str) -> Cell {
    Cell::new(label)
        .fg(Color::Cyan)
        .add_attribute(Attribute::Bold)
}

fn status_cell(status: DiffStatus) -> Cell {
    Cell::new(status.label())
        .fg(status.color())
        .add_attribute(Attribute::Bold)
}

fn colored_list_status(status: DiffStatus) -> String {
    match status {
        DiffStatus::Added => style(status.list_symbol()).green().bold().to_string(),
        DiffStatus::Removed => style(status.list_symbol()).red().bold().to_string(),
        DiffStatus::Changed => style(status.list_symbol()).yellow().bold().to_string(),
    }
}

fn value_cell(value: String) -> Cell {
    Cell::new(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Cursor;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tar::Builder;

    #[test]
    fn hashes_reader_contents() {
        let mut reader = Cursor::new(b"abc");
        let checksum = hash_reader(&mut reader).expect("hash should succeed");
        assert_eq!(
            checksum,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn entry_kind_maps_common_types() {
        assert_eq!(EntryKind::from(EntryType::Regular), EntryKind::File);
        assert_eq!(EntryKind::from(EntryType::Directory), EntryKind::Directory);
        assert_eq!(EntryKind::from(EntryType::Symlink), EntryKind::Symlink);
    }

    #[test]
    fn loads_plain_tar_archives() {
        let archive_path = unique_temp_path("plain.tar");
        let file = File::create(&archive_path).expect("temp archive should be created");
        let mut builder = Builder::new(file);

        let mut header = tar::Header::new_gnu();
        let contents = b"plain tar contents";
        header.set_entry_type(EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(contents.len() as u64);
        header.set_path("hello.txt").expect("path should be set");
        header.set_cksum();
        builder
            .append_data(&mut header, "hello.txt", &contents[..])
            .expect("file should be appended to archive");
        builder.finish().expect("archive should be finalized");
        drop(builder);

        let entries = load_archive(&archive_path, None).expect("plain tar should be readable");
        let entry = entries.get("hello.txt").expect("entry should exist");
        assert_eq!(entry.kind, EntryKind::File);
        assert_eq!(entry.size, contents.len().to_string());
        assert_eq!(
            entry.checksum.as_deref(),
            Some("faff7d8ba57905d8a99355ac95eb024f8c4e699737b09f3aab42b827f4d39060")
        );

        fs::remove_file(&archive_path).expect("temp archive should be removed");
    }

    fn unique_temp_path(suffix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be monotonic")
            .as_nanos();
        std::env::temp_dir().join(format!("tardiff-{nanos}-{suffix}"))
    }
}
