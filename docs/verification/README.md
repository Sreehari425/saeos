# Formal verification

SaeOS uses three complementary techniques for selected, reviewable proof targets:

| Tool | What it checks here | What it does not establish |
| --- | --- | --- |
| [Kani](https://model-checking.github.io/kani/) | Bounded executions of the actual sound-ring Rust module, imported by `tools/kani-ring`. | Unbounded behavior beyond the harness bounds/assumptions, or correctness of unrelated kernel code. |
| [Verus](https://github.com/verus-lang/verus) | Contracts on a small executable allocator-accounting model in `verification/verus/Allocator.rs`. | The production buddy allocator in `kernel/src/mm/buddy.rs` or correspondence to that implementation. |
| [Lean 4](https://lean-lang.org/) | The mathematical mapping policy, single-range planner model, virtual-address/chunk-construction properties, and a conditional exact-coverage theorem in `verification/lean/SaeosVerification/Mapping/`. | That the planner's per-step safety conditions hold for all valid page-aligned inputs, or that the Rust mapping implementation in `kernel/src/mm/mapping_model.rs` refines this model. |

A proof is only as strong as its model, preconditions, and assumptions. In particular, the Verus and Lean checks currently prove properties of separate models, not the production kernel implementations. Connecting a model to its implementation requires a separate refinement/correspondence argument.

## Commands

Run a tool independently:

```sh
make kani
make kani-mapping
make verus-check
make lean-check
```

Run all three proof checks with:

```sh
make verify
```

These verification checks are opt-in and are not prerequisites of `make test`. The default Nix development shell supplies Kani and Verus but does not run proofs automatically.

## Tool setup

On supported `x86_64-linux` hosts, Kani and Verus are packaged directly in this project's Nix flake and included in the development shell:

```sh
nix develop
make kani
make verus-check
```

The flake pins Kani 0.68.0 and reads the Verus release pin from `verification/verus/VERUS_VERSION`. Kani's verifier bundle and matching Rust toolchain, and Verus's release bundle, solver, and matching Rust toolchain, are supplied by Nix; shell activation does not run `cargo kani setup` or install toolchains into the user's home directory. These package definitions currently target Linux x86_64 only.

Outside Nix, install the Kani Cargo subcommand and initialize its toolchain with `cargo install --locked kani-verifier` and `cargo kani setup`. Verus must be installed from the exact release in `verification/verus/VERUS_VERSION` and paired with the Rust toolchain required by that release.

The `make kani` target runs `cargo kani -p kani-ring`. The separate `make kani-mapping` target runs bounded checks against the production pure mapping planner in `kernel/src/mm/mapping_model.rs`, imported directly by `tools/kani-mapping`. Its symbolic coverage harness constrains ranges to at most 8 KiB, with page-aligned starts and lengths and endpoints below the four-level mapping limit; separate fixed-input harnesses exercise 2 MiB and 1 GiB page selection. It checks address conversion boundaries, plan coverage/contiguity, virtual-address mode, leaf-size consistency, and those large-page cases. These proofs cover only the pure range planner under the stated assumptions—not full memory-map input validation, page-table installation, bootloader data validity, hardware behavior, or the production buddy allocator. This target is intentionally opt-in and is not yet included in `make kani`, `make verify`, or `make test`.

### Lean 4

Install [Elan](https://lean-lang.org/install/) and Lake. The project pins Lean in `verification/lean/lean-toolchain`; enter the project directory and let Elan install the pin, then build:

```sh
cd verification/lean
lake build
```

`make lean-check` runs the Lake build from the project directory. The Lean mapping modules cover MMIO base-page selection, huge-page eligibility and priority, fallback policy, a single-range planner model, and direct/identity virtual-address and chunk-construction properties. The exact-coverage theorem is conditional on a per-step safety premise; proving that premise for all page-aligned valid ranges remains future work. These establish properties of the Lean model only; they do not prove that the Rust mapping planner refines the model. Keep Lean dependencies declared in its Lake project and commit any lockfile produced for dependencies.

### Verus outside Nix

Install the prebuilt Verus release listed in `verification/verus/VERUS_VERSION` from the [official releases](https://github.com/verus-lang/verus/releases), then place its `verus` executable on `PATH`. Verus releases require a matching Rust toolchain: run the downloaded verifier once and follow the exact `rustup` toolchain instruction it prints. Keep that toolchain separate from SaeOS's `rust-toolchain.toml` unless a deliberate compatibility change is reviewed.

`make verus-check` invokes Verus on `verification/verus/Allocator.rs`. The version file records the intended verifier release; update it alongside the checked proof when upgrading Verus.

## Adding proofs

Prefer small pure functions and explicit invariants. Keep hardware access, interrupts, and unsafe pointer manipulation outside initial proof targets. For each proof, document the exact source/model, relevant preconditions, bounds, and any assumptions. Do not describe a proof of a model as a proof of production Rust until a correspondence argument exists and is checked.
