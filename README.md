# unpipe

rewrites nix files that use the experimental pipe operators (`|>` and `<|`)
into plain function application for the less pipiously inclined evalutors.

try it out:
```console
$ echo "i |> love |> pipes" | nix run github:turbio/unpipe
pipes (love i)
```

## usage

```console
$ unpipe < in.nix        # stdin to stdout
$ unpipe file.nix ...    # rewrite in place
$ unpipe --check *.nix   # exit 1 if there is a pipe that needs to be un-ed
```

pretty much same cli as nixfmt
