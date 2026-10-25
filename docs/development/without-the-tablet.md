# Working without the tablet

re-fs can be developed and tested against a copy of the tablet's files. Keep that
copy in `mirror/`, which is gitignored.

## Making a mirror

The mirror holds the tablet's `.metadata` and `.content` files, plus sparse files with
each PDF's and EPUB's real size and modification time:

```sh
mkdir mirror
ssh root@10.11.99.1 'cd /home/root/.local/share/remarkable/xochitl && tar cf - *.metadata *.content' | tar xf - -C mirror
ssh root@10.11.99.1 'cd /home/root/.local/share/remarkable/xochitl && stat -c "%n %s %Y" *.pdf *.epub' |
  while read -r n s t; do truncate -s "$s" "mirror/$n"; touch -d "@$t" "mirror/$n"; done
```

## Listing it

```sh
cargo run -p re-fs -- ls --mirror mirror -R
tools/check-mirror.sh                     # compare with an independent Python model
```

## Mounting it

A native agent serves a copy of the mirror, so the whole client can be tried,
writes included:

```sh
cp -r mirror /tmp/mirror-copy
cargo run -p re-fsd -- --dir /tmp/mirror-copy --listen 127.0.0.1:7746
cargo run -p re-fs -- mount ~/rm --tcp 127.0.0.1:7746
```

Work on a copy: writes change it like the tablet's real folder.
