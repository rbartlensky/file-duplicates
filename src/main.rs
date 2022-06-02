use file_duplicates::Params;

use std::io::Write;

const HELP: &str = "\
fdup 0.2 -- Find duplicate files based on their hash.

USAGE:
  fdup [FLAGS] [OPTIONS] ROOT
FLAGS:
  -h, --help               Prints help information.
  -r, --remove             Interactively remove duplicate files.
OPTIONS:
  -l, --lower-limit LIMIT  Files whose size is under <LIMIT> are ignored [default: 1 MiB].
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
    let mut interactive_removal = false;

    let mut free = pargs.free_from_str::<String>()?;
    if free == "--remove" || free == "-r" {
        interactive_removal = true;
        free = pargs.free_from_str::<String>()?;
    }
    let params = Params { lower_limit, root: free.into() };
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
    for (hash, paths) in stats.duplicates {
        if paths.len() > 1 {
            println!("Hash: {}", hash);
            for (size, path) in &paths {
                println!("-> size: {}, file: '{}'", format_bytes(*size), path.display());
                dup_bytes += size;
            }
            dup_bytes -= paths[0].0;
        }
    }
    println!(
        "Processed {} files (total of {})",
        stats.total_files_processed,
        format_bytes(stats.total_bytes_processed)
    );
    println!("Duplicate files take up {} of space on disk.", format_bytes(dup_bytes));
}

fn remove_file(path: &std::path::Path) {
    if let Err(e) = std::fs::remove_file(path) {
        eprintln!("failed to remove '{}': {}", path.display(), e);
    }
}

fn interactive_removal(
    stats: file_duplicates::Stats,
    mut stdin: impl std::io::BufRead,
) -> std::io::Result<()> {
    for (hash, mut paths) in stats.duplicates {
        if paths.len() > 1 {
            println!("Hash: {}", hash);
            paths.sort();
            let mut i = 0;
            let mut j = 1;
            while i < j && j < paths.len() {
                let (size1, path1) = &paths[i];
                let (size2, path2) = &paths[j];
                let mut choice = String::with_capacity(3);
                let mut read = true;
                while read {
                    print!(
                        "(1) {} (size {})\n(2) {} (size {})\nRemove (0 to skip): ",
                        path1.display(),
                        format_bytes(*size1),
                        path2.display(),
                        format_bytes(*size2),
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
                        "0" => {
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
        interactive_removal(stats, std::io::stdin().lock())?;
    } else {
        print_stats(stats);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{fs::File, io::Cursor, path::Path};

    fn build_tree(files: &[(&str, &[u8])]) -> tempfile::TempDir {
        let tmpdir = tempfile::tempdir().unwrap();
        for (path, data) in files {
            let mut file = File::create(tmpdir.path().join(path)).unwrap();
            file.write_all(data).unwrap();
        }
        tmpdir
    }

    fn do_removal(choice: &[u8]) -> tempfile::TempDir {
        let dir = build_tree(&[("a", b"a"), ("a2", b"a")]);
        let stats =
            file_duplicates::find(&Params { lower_limit: 0, root: dir.path().to_owned() }).unwrap();
        let input = Cursor::new(choice);
        interactive_removal(stats, input).unwrap();
        dir
    }

    fn do_check(dir: &Path, files: &[(&str, bool)]) {
        for (file, exists) in files {
            let file = dir.join(file);
            assert_eq!(file.exists(), *exists, "{:?}", file);
        }
    }

    #[test]
    fn remove_file_1() {
        let dir = do_removal(b"1\n");
        let files = [("a", false), ("a2", true)];
        do_check(dir.path(), &files);
    }

    #[test]
    fn remove_file_2() {
        let dir = do_removal(b"2\n");
        let files = [("a", true), ("a2", false)];
        do_check(dir.path(), &files);
    }

    #[test]
    fn remove_none() {
        let dir = do_removal(b"0\n");
        let files = [("a", true), ("a2", true)];
        do_check(dir.path(), &files);
    }
}
