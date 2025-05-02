use file_duplicates::{HashDb, Params};
use std::fs::File;
use std::path::{Path, PathBuf};

use std::io::{self, BufRead, BufReader, Write};

const HELP: &str = "\
fdup 0.4.1 -- Find duplicate files based on their hash.

USAGE:
  fdup [FLAGS] [OPTIONS] PATH...
FLAGS:
  -h, --help                   Prints help information.
  -r, --remove                 Interactively remove duplicate files.
  --remove-with-same-filename  Remove duplicate files that have the same filename.
  --remove-paranoid            Remove duplicate files, but also check if they have the same content.
OPTIONS:
  -l, --lower-limit LIMIT  Files whose size is under <LIMIT> are ignored [default: 1 MiB].
  --database        PATH   Path to the hash database [default: $HOME/.config/fdup.db].
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
    params: Params,
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
        .unwrap_or_else(|| 1 * 1024);

    // fallback to `$HOME/.config/fdup.db` if `--database` is not present
    let db = match pargs
        .opt_value_from_os_str::<_, _, &str>("--database", |s| Ok(PathBuf::from(s)))?
    {
        Some(db) => db,
        None => home::home_dir().map(|h| h.join(".config").join("fdup.db")).ok_or(
            pico_args::Error::OptionWithoutAValue("'--database' is required if $HOME is not set"),
        )?,
    };
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
        let params = Params::new(lower_limit, roots, db);
        Ok(Some(Args { params, remove }))
    }
}

fn format_bytes(bytes: u64) -> String {
    let unit = byte_unit::Byte::from_u64(bytes).get_appropriate_unit(byte_unit::UnitType::Binary);

    format!("{unit:.2}")
}

fn print_stats(stats: file_duplicates::Stats) {
    let mut dup_bytes = 0;
    println!("The following duplicate files have been found:");
    for ((size, hash), paths) in stats.duplicates {
        if paths.len() > 1 {
            dup_bytes += paths.len() as u64 * size;
            println!("Hash: {}", hash);
            for path in &paths {
                println!("-> size: {}, file: '{}'", format_bytes(size), path.display());
            }
        }
    }
    println!(
        "Processed {} files (total of {})",
        stats.total_files_processed,
        format_bytes(stats.total_bytes_processed)
    );
    println!("Duplicate files take up {} of space on disk.", format_bytes(dup_bytes));
}

fn remove_file(path: &std::path::Path, db: &HashDb) {
    if let Err(e) = std::fs::remove_file(path) {
        eprintln!("failed to remove '{}': {}", path.display(), e);
    } else {
        db.remove(path).unwrap();
    }
}

fn interactive_removal(
    db: &Path,
    stats: file_duplicates::Stats,
    mut stdin: impl std::io::BufRead,
) -> io::Result<()> {
    let db = file_duplicates::HashDb::try_new(db).unwrap();
    for ((size, hash), mut paths) in stats.duplicates {
        if paths.len() > 1 {
            println!("Hash: {}", hash);
            paths.sort();
            let mut i = 0;
            let mut j = 1;
            while i < j && j < paths.len() {
                let path1 = &paths[i];
                let path2 = &paths[j];
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
                            remove_file(path1, &db);
                            i = j;
                            j += 1;
                        }
                        "2" => {
                            remove_file(path2, &db);
                            j += 1;
                        }
                        _ => read = true,
                    }
                }
            }
        }
    }
    Ok(())
}

fn same_filename_removal(db: &Path, stats: file_duplicates::Stats) {
    let db = file_duplicates::HashDb::try_new(db).unwrap();
    for (_, mut paths) in stats.duplicates {
        if paths.len() > 1 {
            paths.sort();
            for dup_path in &paths[1..] {
                if dup_path.file_name() == paths[0].file_name() {
                    println!(
                        "Removing '{}' (duplicate of '{}')",
                        dup_path.display(),
                        paths[0].display()
                    );
                    remove_file(dup_path, &db);
                }
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

fn paranoid_removal(db: &Path, stats: file_duplicates::Stats) {
    let db = file_duplicates::HashDb::try_new(db).unwrap();
    for (_, mut paths) in stats.duplicates {
        if paths.len() > 1 {
            paths.sort();
            for dup_path in &paths[1..] {
                match same_content(&paths[0], dup_path) {
                    Ok(true) => {
                        println!(
                            "Removing '{}' (duplicate of '{}')",
                            dup_path.display(),
                            paths[0].display()
                        );
                        remove_file(dup_path, &db);
                    }
                    Ok(false) => {}
                    Err(e) => eprintln!(
                        "failed to compare '{}' to '{}': {:?}",
                        dup_path.display(),
                        paths[0].display(),
                        e
                    ),
                }
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    let args = match parse_args()? {
        Some(args) => args,
        None => return Ok(()),
    };
    println!("Directories: {:?}", args.params.roots());
    let stats = file_duplicates::find(&args.params)?;
    match args.remove {
        Some(RemovalKind::Interactive) => {
            interactive_removal(args.params.db_path(), stats, std::io::stdin().lock())?
        }
        Some(RemovalKind::SameFilename) => same_filename_removal(args.params.db_path(), stats),
        Some(RemovalKind::Paranoid) => paranoid_removal(args.params.db_path(), stats),
        None => print_stats(stats),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{fs::File, io::Cursor, path::Path};
    use tempfile::{NamedTempFile, TempDir};

    type Files<'a> = [(&'a str, &'a [u8])];

    struct Context {
        dir: TempDir,
        db: NamedTempFile,
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

    fn do_remove(dir: TempDir, f: impl FnOnce(&Path, file_duplicates::Stats)) -> Context {
        let db = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        let stats = file_duplicates::find(&Params::new(
            0,
            vec![dir.path().to_owned()],
            db.path().to_owned(),
        ))
        .unwrap();
        f(db.path(), stats);
        Context { dir, db }
    }

    fn do_removal(choice: &[u8]) -> Context {
        let dir = tempfile::tempdir().unwrap();
        build_tree(dir.path(), &[("a", b"a"), ("a2", b"a")]);
        do_remove(dir, |db, stats| {
            let input = Cursor::new(choice);
            interactive_removal(db, stats, input).unwrap();
        })
    }

    fn do_check(ctx: Context, files: &[(&str, bool)]) {
        let db = HashDb::try_new(ctx.db.path()).unwrap();
        for (file, exists) in files {
            let file = ctx.dir.path().join(file);
            assert_eq!(file.exists(), *exists, "{:?}", file);
            // also check if the db got updated properly
            assert_eq!(db.select(&file).unwrap().is_some(), *exists);
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
        let ctx = do_remove(dir, |db, stats| same_filename_removal(db, stats));
        let files = [("a/a1", true), ("a/b", true), ("b/a2", true), ("b/b", false)];
        do_check(ctx, &files);
    }

    #[test]
    fn paranoid_removal_removes_duplicates() {
        let dir = build_nested_tree(&[
            ("a", &[("a1", b"a1"), ("b", b"b")]),
            ("b", &[("a2", b"a1"), ("b", b"b")]),
        ]);
        let ctx = do_remove(dir, |db, stats| paranoid_removal(db, stats));
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
