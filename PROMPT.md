I want a small/simple rust library that can turn a puredata patch into a JSON AST, similar to how javascript/jsx/markdown parsers work.

My ultimate goal is to generate code from puredata patches, similar to HVCC, but I feel like HVCC is a bit convoluted & complicated, and hard to expand, and I have other code-targets it doesn't support.

I could expose the library as wasm for JS, and also native, and then process the AST in any language to generate different kinds of code. It should be able to work without a filesystem, so I might need a way to tell the parser how to load things (for example sub-patches and native extensions.)

I feel like a puredata patch is made up of these things, but it should be thoroughly researched:

- patch is objects defined and connected to each other
- types of patches are subpatch/native extensions/vanilla

On a high-level:

- convert pd patches to AST (import)
- convert AST to pd patches (export)
- examples that operate on AST as input, and output something.

I'd also like to be able convert a patch to AST 1-time, and operate on the AST instead of the patch, in realtime targets (for example in an audio-worklet on the web.)

Some good example AST output targets:

- AST back to puredata (export, rountrip)
- Faust, which can generate many other formats (native standalone/puredata/vst/ladspa/etc)
- Teensy Audio Library (C code)
