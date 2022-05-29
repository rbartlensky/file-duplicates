use file_duplicates::Params;

const HELP: &str = "\
fdup 0.1 -- Find duplicate files based on their hash.

USAGE:
  fdup [FLAGS] [OPTIONS] ROOT
FLAGS:
  -h, --help               Prints help information.
OPTIONS:
  -l, --lower-limit LIMIT  Files whose size is under <LIMIT> are ignored [default: 1 MiB].
ARGS:
  <ROOT>                   Where to start the search from.
";

fn parse_args() -> Result<Option<Params>, pico_args::Error> {
    let mut pargs = pico_args::Arguments::from_env();

    if pargs.contains(["-h", "--help"]) {
        print!("{}", HELP);
        return Ok(None);
    }

    let params = Params {
        lower_limit: pargs
            .opt_value_from_fn(["-l", "--lower-limit"], |s| byte_unit::Byte::from_str(s))?
            .map(|b| b.get_bytes())
            .unwrap_or_else(|| byte_unit::n_mib_bytes(1) as u64),
        root: pargs.free_from_str()?,
    };

    let remaining = pargs.finish();
    if !remaining.is_empty() {
        Err(pico_args::Error::ArgumentParsingFailed {
            cause: format!(": unknown arguments {:?}", remaining),
        })
    } else {
        Ok(Some(params))
    }
}

fn format_bytes(bytes: u64) -> String {
    byte_unit::Byte::from_bytes(bytes).get_appropriate_unit(true).format(2)
}

fn main() -> anyhow::Result<()> {
    let args = match parse_args()? {
        Some(args) => args,
        None => return Ok(()),
    };

    let stats = file_duplicates::find(&args)?;
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
    Ok(())
}
