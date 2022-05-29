# file-duplicates

A quick way to find out if you have duplicate files in a directory.

## Goals

The goals of this project are very straightforward: build an efficient tool
that can find duplicate files on disk based on hashing the contents of files.

Features:
 * multi-threaded search
 * skip small files
 * low memory footprint
 * small dependency tree (and fast compile times)

Future goals:
 * interactive removal of duplicates
 * more performance optimisations
 * more filters

## Demo

```
$ cargo run --release -- ./
The following duplicate files have been found:
Hash: 4c8cd7a46dc0581ca116b81a8bacc69e4be215391a46b8f13fda0baad9d6ea74
-> size: 4.26 MiB, file: './target/debug/build/typenum-0d421246c9930e76/build_script_main-0d421246c9930e76'
-> size: 4.26 MiB, file: './target/debug/build/typenum-0d421246c9930e76/build-script-main'
< truncated for demo purposes>
Hash: fc1b93e522c3ba0bc9b98166a9ac98582b6f34cf844af9ea8e0cb96a18d812e3
-> size: 9.63 MiB, file: './target/debug/fdup'
-> size: 9.63 MiB, file: './target/debug/deps/fdup-0a076dc9c883b798'
Processed 59 files (total of 222.37 MiB)
Duplicate files take up 76.41 MiB of space on disk.
```

Make sure to run with `--help` for a more detailed description.

## Building

Like with any cargo project, use `cargo build` to build the project.
