use file_duplicates::{HashDb, Params};
use std::path::PathBuf;

use std::io::Write;

const HELP: &str = "\
fdup 0.3 -- Find duplicate files based on their hash.

USAGE:
  fdup [FLAGS] [OPTIONS] ROOT
FLAGS:
  -h, --help               Prints help information.
  -r, --remove             Interactively remove duplicate files.
OPTIONS:
  -l, --lower-limit LIMIT  Files whose size is under <LIMIT> are ignored [default: 1 MiB].
  --database        PATH   Path to the hash database [default: $HOME/.config/fdup.db].
ARGS:
  <ROOT>                   Where to start the search from.
";

struct Args {
    interactive_removal: bool,
    params: Params,
}

fn parse_args() -> Result<Option<Args>, pico_args::Error> {
    let mut pargs = pico_args::Arguments::from_env();

    if pargs.contains(["-h", "--help"]) {
        print!("{}", HELP);
        return Ok(None);
    }

    let lower_limit = pargs
        .opt_value_from_fn(["-l", "--lower-limit"], |s| byte_unit::Byte::from_str(s))?
        .map(|b| b.get_bytes())
        .unwrap_or_else(|| byte_unit::n_mib_bytes(1) as u64);

    // fallback to `$HOME/.config/fdup.db` if `--database` is not present
    let db = match pargs
        .opt_value_from_os_str::<_, _, &str>("--database", |s| Ok(PathBuf::from(s)))?
    {
        Some(db) => db,
        None => home::home_dir().map(|h| h.join(".config").join("fdup.db")).ok_or(
            pico_args::Error::OptionWithoutAValue("`--database` is required if $HOME is not set"),
        )?,
    };
    let mut interactive_removal = false;

    let mut free = pargs.free_from_str::<String>()?;
    if free == "--remove" || free == "-r" {
        interactive_removal = true;
        free = pargs.free_from_str::<String>()?;
    }
    let params = Params { lower_limit, root: free.into(), db };
    let remaining = pargs.finish();
    if !remaining.is_empty() {
        Err(pico_args::Error::ArgumentParsingFailed {
            cause: format!(": unknown arguments {:?}", remaining),
        })
    } else {
        Ok(Some(Args { params, interactive_removal }))
    }
}

fn format_bytes(bytes: u64) -> String {
    byte_unit::Byte::from_bytes(bytes).get_appropriate_unit(true).format(2)
}

fn print_stats(stats: file_duplicates::Stats) {
    let mut dup_bytes = 0;
    println!("The following duplicate files have been found:");
    for ((size, hash), paths) in stats.duplicates {
        dup_bytes += paths.len() as u64 * size;
        if paths.len() > 1 {
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
    db: PathBuf,
    stats: file_duplicates::Stats,
    mut stdin: impl std::io::BufRead,
) -> std::io::Result<()> {
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

fn main() -> anyhow::Result<()> {
    let args = match parse_args()? {
        Some(args) => args,
        None => return Ok(()),
    };

    println!("Directory: '{}'", args.params.root.display());
    let stats = file_duplicates::find(&args.params)?;
    if args.interactive_removal {
        interactive_removal(args.params.db, stats, std::io::stdin().lock())?;
    } else {
        print_stats(stats);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{fs::File, io::Cursor};
    use tempfile::{NamedTempFile, TempDir};

    struct Context {
        dir: TempDir,
        db: NamedTempFile,
    }

    fn build_tree(files: &[(&str, &[u8])]) -> tempfile::TempDir {
        let tmpdir = tempfile::tempdir().unwrap();
        for (path, data) in files {
            let mut file = File::create(tmpdir.path().join(path)).unwrap();
            file.write_all(data).unwrap();
        }
        tmpdir
    }

    fn do_removal(choice: &[u8]) -> Context {
        let dir = build_tree(&[("a", b"a"), ("a2", b"a")]);
        let db = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        let stats = file_duplicates::find(&Params {
            lower_limit: 0,
            root: dir.path().to_owned(),
            db: db.path().to_owned(),
        })
        .unwrap();
        let input = Cursor::new(choice);
        interactive_removal(db.path().to_owned(), stats, input).unwrap();
        Context { dir, db }
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
}
