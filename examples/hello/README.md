# hello — the minimal working example

Two packages: a static library (`libgreet`) and an executable that uses it
(`app`). Demonstrates that `dowel` really compiles C.

```sh
cd app
dowel check
dowel build
./.dowel/build/*/bin/app

cd ../libgreet
dowel test        # build and run test.greet_test
```

What to look at:

- `libgreet/include` is in `public.includes`, `libgreet/src` in
  `private.includes`. `app` can see headers from the former but not the
  latter
- `GREET_API` in `public.defines` also affects the compilation of `app`
- `flags` switch per configuration via `match cfg.opt`; confirm with
  `dowel build --config=release`
- `[test.greet_test]` in `libgreet` uses only the public headers.
  `dowel test` judges pass/fail by exit status

Propagation paths can be traced with `dowel why`:

```sh
dowel why app:app includes
dowel graph --kind=action
```

This example is built and checked by `crates/dowel-cli/tests/example.rs`.
