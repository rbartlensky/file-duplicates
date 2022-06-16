# file-duplicates

A quick way to find out if you have duplicate files in a directory.

`fdup` stores computed hashes on disk, so that if you run the command again
there is no need to recompute hashes for files that haven't changed since the
last run.

## Goals

The goals of this project are very straightforward: build an efficient tool
that can find duplicate files on disk based on hashing the contents of files.

Features:
 * multi-threaded search
 * interactive removal of duplicates
 * skip small files
 * low memory footprint
 * small-ish dependency tree (and fast compile times)
 ** sqlite is the slowest to compile since we bundle it
 * hash storage for fast re-runs

Future goals:
 * more performance optimisations
 * more filters

## Demo

```
$ cargo run --release -- ./
Directory: './'
The following duplicate files have been found:
Hash: 4c8cd7a46dc0581ca116b81a8bacc69e4be215391a46b8f13fda0baad9d6ea74
-> size: 4.26 MiB, file: './target/debug/build/typenum-0d421246c9930e76/build_script_main-0d421246c9930e76'
-> size: 4.26 MiB, file: './target/debug/build/typenum-0d421246c9930e76/build-script-main'
< truncated for demo purposes >
Hash: fc1b93e522c3ba0bc9b98166a9ac98582b6f34cf844af9ea8e0cb96a18d812e3
-> size: 9.63 MiB, file: './target/debug/fdup'
-> size: 9.63 MiB, file: './target/debug/deps/fdup-0a076dc9c883b798'
Processed 59 files (total of 222.37 MiB)
Duplicate files take up 76.41 MiB of space on disk.

$ cargo run --release -- -r ./
Directory: './'
Hash: 784e6aa2a21a83d03f485578e226125049c6e37c23a5c5e43a43b64bf10a8df3
(1) ./target/release/build/typenum-4dfd976f69348bc2/build-script-main (size 3.81 MiB)
(2) ./target/release/build/typenum-4dfd976f69348bc2/build_script_main-4dfd976f69348bc2 (size 3.81 MiB)
Remove (s to skip): 2
< ... >
```

Make sure to run with `--help` for a more detailed description.

## Building

Like with any cargo project, use `cargo build` to build the project.
