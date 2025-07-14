use duped::{ContentLimit, Deduper, DeduperResult};

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const HELP: &str = "\
duped 0.1.0 -- Find duplicate files based on their hash.

USAGE:
  fdup [FLAGS] [OPTIONS] PATH...
FLAGS:
  -h, --help                   Prints help information.
  -r, --remove                 Interactively remove duplicate files.
  --remove-with-same-filename  Remove duplicate files that have the same filename.
  --remove-paranoid            Remove duplicate files, but also check if they have the same content.
OPTIONS:
  -l, --lower-limit LIMIT  Files whose size is under <LIMIT> are ignored [default: 1 MiB].
ARGS:
  <PATH...>                Where to start the search from (can be specified multiple times).
";

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
enum RemovalKind {
    Interactive,
    SameFilename,
    Paranoid,
}

impl RemovalKind {
    fn as_option(&self) -> &'static str {
        match self {
            RemovalKind::Interactive => "--remove",
            RemovalKind::SameFilename => "--remove-with-same-filename",
            RemovalKind::Paranoid => "--remove-paranoid",
        }
    }

    fn from_option(opt: &str) -> Option<Self> {
        match opt {
            "--remove" | "-r" => Some(RemovalKind::Interactive),
            "--remove-with-same-filename" => Some(RemovalKind::SameFilename),
            "--remove-paranoid" => Some(RemovalKind::Paranoid),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct Args {
    remove: Option<RemovalKind>,
    deduper: Deduper,
    content_limit: ContentLimit,
}

fn parse_args() -> Result<Option<Args>, pico_args::Error> {
    let mut pargs = pico_args::Arguments::from_env();

    if pargs.contains(["-h", "--help"]) {
        print!("{}", HELP);
        return Ok(None);
    }

    let lower_limit = pargs
        .opt_value_from_fn(["-l", "--lower-limit"], |s| byte_unit::Byte::parse_str(s, false))?
        .map(|b| b.as_u64())
        .unwrap_or_else(|| 1024);

    let remaining = pargs.finish();
    let mut remove = None;
    let mut roots: Vec<PathBuf> = vec![];
    for arg in &remaining {
        // if we can't parse the argument as UTF-8, we will just assume we are dealing with a PATH argument
        if let Some(arg) = arg.to_str() {
            if let Some(kind) = RemovalKind::from_option(arg) {
                // did we process a root already? if so, then we have something like
                // "./foo --remove ./bar" which is incorrect
                if !roots.is_empty() {
                    return Err(pico_args::Error::ArgumentParsingFailed {
                        cause: format!(
                            "cannot specify '{}' after a <PATH> argument",
                            kind.as_option()
                        ),
                    });
                }
                let old = remove.replace(kind);
                match old {
                    Some(inner_kind) if inner_kind == kind => {
                        return Err(pico_args::Error::ArgumentParsingFailed {
                            cause: format!("'{}' passed multiple times", kind.as_option()),
                        })
                    }
                    Some(inner_kind) => {
                        return Err(pico_args::Error::ArgumentParsingFailed {
                            cause: format!(
                                "'{}' conflicts with '{}'",
                                kind.as_option(),
                                inner_kind.as_option()
                            ),
                        })
                    }
                    None => continue,
                }
            }
        }
        roots.push(arg.into());
    }
    if roots.is_empty() {
        Err(pico_args::Error::ArgumentParsingFailed {
            cause: "'<PATH>' argument is missing".into(),
        })
    } else {
        let deduper = Deduper::builder(roots).build();
        let content_limit = ContentLimit::no_limit().with_lower_limit(lower_limit);
        Ok(Some(Args { deduper, remove, content_limit }))
    }
}

fn format_bytes(bytes: u64) -> String {
    let unit = byte_unit::Byte::from_u64(bytes).get_appropriate_unit(byte_unit::UnitType::Binary);

    format!("{unit:.2}")
}

fn print_stats(duplicates: DeduperResult) {
    let mut dup_bytes = 0;
    println!("The following duplicate files have been found:");
    for (hash, paths) in duplicates.duplicates() {
        println!("Hash: {}", hash);
        let size = paths.file_size();
        for entry in paths.iter() {
            dup_bytes += size;
            println!("-> size: {}, file: '{}'", format_bytes(size), entry.display());
        }
    }
    println!("Duplicate files take up {} of space on disk.", format_bytes(dup_bytes));
}

fn remove_file(path: &std::path::Path) {
    if let Err(e) = std::fs::remove_file(path) {
        eprintln!("failed to remove '{}': {}", path.display(), e);
    }
}

fn interactive_removal(
    duplicates: DeduperResult,
    mut stdin: impl std::io::BufRead,
) -> io::Result<()> {
    for (hash, entries) in duplicates.duplicates() {
        let size = entries.file_size();
        println!("Hash: {}", hash);
        let mut entries = entries.iter().map(|e| e.to_owned()).collect::<Vec<_>>();
        entries.sort();
        let mut i = 0;
        let mut j = 1;
        while i < j && j < entries.len() {
            let path1 = &entries[i];
            let path2 = &entries[j];
            let mut choice = String::with_capacity(3);
            let mut read = true;
            while read {
                print!(
                    "(1) {} (size {})\n(2) {} (size {})\nRemove (s to skip): ",
                    path1.display(),
                    format_bytes(size),
                    path2.display(),
                    format_bytes(size),
                );
                if let Err(e) = std::io::stdout().flush() {
                    eprintln!("failed to flush to stdout: {}", e);
                    return Err(e);
                }
                if let Err(e) = stdin.read_line(&mut choice) {
                    eprintln!("failed to read from stdin: {}", e);
                    return Err(e);
                }
                println!();
                read = false;
                match choice.trim() {
                    "s" => {
                        i = j + 1;
                        j += 2;
                    }
                    "1" => {
                        remove_file(path1);
                        i = j;
                        j += 1;
                    }
                    "2" => {
                        remove_file(path2);
                        j += 1;
                    }
                    _ => read = true,
                }
            }
        }
    }
    Ok(())
}

fn same_filename_removal(duplicates: DeduperResult) {
    for (_, entries) in duplicates.duplicates() {
        let mut entries = entries.iter().map(|e| e.to_owned()).collect::<Vec<_>>();
        entries.sort();
        for dup_path in &entries[1..] {
            if dup_path.file_name() == entries[0].file_name() {
                println!(
                    "Removing '{}' (duplicate of '{}')",
                    dup_path.display(),
                    entries[0].display()
                );
                remove_file(dup_path);
            }
        }
    }
}

fn same_content(p1: &Path, p2: &Path) -> io::Result<bool> {
    let mut reader1 = BufReader::new(File::open(p1)?);
    let mut reader2 = BufReader::new(File::open(p2)?);
    loop {
        // XXX: put the other one in another thread?
        let data1 = reader1.fill_buf()?;
        let data2 = reader2.fill_buf()?;
        if data1 != data2 {
            return Ok(false);
        }
        if data1.is_empty() {
            break;
        }
        let len1 = data1.len();
        reader1.consume(len1);
        let len2 = data2.len();
        reader2.consume(len2);
    }
    Ok(true)
}

fn paranoid_removal(duplicates: DeduperResult) {
    for (_, entries) in duplicates.duplicates() {
        let mut entries = entries.iter().map(|e| e.to_owned()).collect::<Vec<_>>();
        entries.sort();
        for dup_path in &entries[1..] {
            match same_content(&entries[0], dup_path) {
                Ok(true) => {
                    println!(
                        "Removing '{}' (duplicate of '{}')",
                        dup_path.display(),
                        entries[0].display()
                    );
                    remove_file(dup_path);
                }
                Ok(false) => {}
                Err(e) => eprintln!(
                    "failed to compare '{}' to '{}': {:?}",
                    dup_path.display(),
                    entries[0].display(),
                    e
                ),
            }
        }
    }
}

#[derive(Default)]
struct FindHook {
    last_round: AtomicUsize,
    count: AtomicUsize,
}

impl duped::DeduperFindHook for FindHook {
    fn files_selected(&self, size: usize) {
        self.last_round.store(size, Ordering::Relaxed);
        self.count.store(0, Ordering::Relaxed);
        print!("0/{size}\r");
    }

    fn entry_processed(&self, _: duped::blake3::Hash, _: &duped::FileEntry) {
        let old = self.count.fetch_add(1, Ordering::Relaxed) + 1;
        let n = self.last_round.load(Ordering::Relaxed);
        print!("{old}/{n}\r");
    }
}

fn main() -> anyhow::Result<()> {
    let args = match parse_args()? {
        Some(args) => args,
        None => return Ok(()),
    };
    println!("Directories: {:?}", args.deduper.roots());
    let stats = args.deduper.find(args.content_limit, FindHook::default())?;
    match args.remove {
        Some(RemovalKind::Interactive) => interactive_removal(stats, std::io::stdin().lock())?,
        Some(RemovalKind::SameFilename) => same_filename_removal(stats),
        Some(RemovalKind::Paranoid) => paranoid_removal(stats),
        None => print_stats(stats),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{fs::File, io::Cursor, path::Path};
    use tempfile::TempDir;

    type Files<'a> = [(&'a str, &'a [u8])];

    struct Context {
        dir: TempDir,
    }

    fn build_tree(dir: &Path, files: &Files<'_>) {
        for (path, data) in files {
            let mut file = File::create(dir.join(path)).unwrap();
            file.write_all(data).unwrap();
        }
    }

    fn build_nested_tree(files: &[(&str, &Files<'_>)]) -> tempfile::TempDir {
        let tmpdir = tempfile::tempdir().unwrap();
        for (dir, files) in files {
            let dir = tmpdir.path().join(dir);
            std::fs::create_dir(&dir).unwrap();
            build_tree(&dir, files);
        }
        tmpdir
    }

    fn do_remove(dir: TempDir, f: impl FnOnce(DeduperResult)) -> Context {
        let stats = duped::Deduper::builder(vec![dir.path().to_owned()]).build();
        f(stats.find(ContentLimit::no_limit(), duped::NoopFindHook).unwrap());
        Context { dir }
    }

    fn do_removal(choice: &[u8]) -> Context {
        let dir = tempfile::tempdir().unwrap();
        build_tree(dir.path(), &[("a", b"a"), ("a2", b"a")]);
        do_remove(dir, |stats| {
            let input = Cursor::new(choice);
            interactive_removal(stats, input).unwrap();
        })
    }

    fn do_check(ctx: Context, files: &[(&str, bool)]) {
        for (file, exists) in files {
            let file = ctx.dir.path().join(file);
            assert_eq!(file.exists(), *exists, "{:?}", file);
        }
    }

    #[test]
    fn remove_file_1() {
        let ctx = do_removal(b"1\n");
        let files = [("a", false), ("a2", true)];
        do_check(ctx, &files);
    }

    #[test]
    fn remove_file_2() {
        let ctx = do_removal(b"2\n");
        let files = [("a", true), ("a2", false)];
        do_check(ctx, &files);
    }

    #[test]
    fn remove_none() {
        let ctx = do_removal(b"s\n");
        let files = [("a", true), ("a2", true)];
        do_check(ctx, &files);
    }

    #[test]
    fn same_filenames_deleted() {
        let dir = build_nested_tree(&[
            ("a", &[("a1", b"a1"), ("b", b"b")]),
            ("b", &[("a2", b"a1"), ("b", b"b")]),
        ]);
        let ctx = do_remove(dir, same_filename_removal);
        let files = [("a/a1", true), ("a/b", true), ("b/a2", true), ("b/b", false)];
        do_check(ctx, &files);
    }

    #[test]
    fn paranoid_removal_removes_duplicates() {
        let dir = build_nested_tree(&[
            ("a", &[("a1", b"a1"), ("b", b"b")]),
            ("b", &[("a2", b"a1"), ("b", b"b")]),
        ]);
        let ctx = do_remove(dir, paranoid_removal);
        let files = [("a/a1", true), ("a/b", true), ("b/a2", false), ("b/b", false)];
        do_check(ctx, &files);
    }

    #[test]
    fn same_content_works() {
        let dir = tempfile::tempdir().unwrap();
        build_tree(dir.path(), &[("a", b"a"), ("a2", b"a"), ("a3", b"b")]);
        let a = dir.path().join("a");
        let a2 = dir.path().join("a2");
        let a3 = dir.path().join("a3");
        assert!(same_content(&a, &a2).unwrap());
        assert!(!same_content(&a, &a3).unwrap());
        assert!(!same_content(&a2, &a3).unwrap());
        assert!(same_content(&a3, &a3).unwrap());
    }
}
