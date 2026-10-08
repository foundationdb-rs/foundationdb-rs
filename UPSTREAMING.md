# Upstreaming foundationdb-rs into FoundationDB

Status: draft, questions for discussion with the FoundationDB maintainers.
Author: Pierre Zemb (maintainer of foundationdb-rs).
Last updated: 2026-10-08.

## Intent

I picked up foundationdb-rs in 2021, when it had been unmaintained for about a year: nobody
could merge, CI was broken, and users were still downloading it every day. Since then I have
kept it alive, mostly on my own, first on evenings and weekends and later with time from my
employer. Seeing the Rust bindings become an official part of FoundationDB would be the best
outcome for this work, and I would be really glad to land the import myself: prepare the
last foundationdb-rs release, open the import PR, and see it through. I also want to keep
maintaining the bindings upstream afterwards (reviews, releases, bindingtester and
simulation coverage). The questions below are about how to make that work within the
project's process.

## Key decisions

Most of the questions below follow from these, with our proposal for each:

- **Shape:** in-tree `bindings/rust` (question 1).
- **Branches and versioning:** Rust lives on `main` only, not on release branches, keeps its
  own semver, and one crate keeps supporting several API versions (questions 22 and 25).
- **Scope:** everything except recipes and profiling, simulation included (section 2).
- **Access:** external committer status scoped to `bindings/rust`, and CI trigger rights
  (question 10).
- **Publishing:** who publishes to crates.io and holds the token (question 23).

## Background

foundationdb-rs is the Rust client for FoundationDB, layered over `libfdb_c`.

- **History.** Started by Benjamin Fry in 2018. The repository later moved under a company
  org (Clikengo), and became unmaintained there: nobody could merge, CI was broken, PRs went
  unanswered for months. On 2021-12-08 we hard-forked it into a neutral GitHub org,
  `foundationdb-rs`, on purpose, so that it would no longer depend on a single company.
  The first release from the new home (0.6) shipped on 2022-04-13.
- **Usage.** About 15.2M downloads on crates.io (341k over the last 90 days), 16 versions,
  17 reverse dependencies (Apache OpenDAL among them), 235 GitHub stars. Known production
  users include Clever Cloud.
- **Testing.** The official bindingtester runs against the Python bindings every hour on
  GitHub Actions, and on demand for PRs. Rust workloads can run inside the FDB deterministic
  simulator through the C workload API (`foundationdb-simulation`).
- **Maintenance.** About 40 contributors over time, but a single active maintainer for the
  last years. Trevor Clinkenbeard has recently contributed a large set of soundness and
  spec-compliance fixes.

## Where we are

The Rust binding coming into apple/foundationdb is foundationdb-rs, the project I have
maintained since reviving it in 2021, not a new codebase, and this import is its next step.
Trevor Clinkenbeard recently contributed to it, with a large set of soundness and
spec-compliance fixes, and with a prototype of the in-tree integration in
[apple/foundationdb#14187](https://github.com/apple/foundationdb/pull/14187) (CMake,
bindingtester, CI, a C client change). Production users increasingly rely on Rust bindings,
and an official home makes sense for them. As maintainer, I will land the pending fixes in
foundationdb-rs, cut a release so current users get them, then open the import from that
release tag, reusing the integration from #14187, and keep maintaining the binding
upstream. That keeps one owner and one history behind the binding.

What #14187 shows about the integration:

- It imports a snapshot of foundationdb-rs `f0eda232` (2026-09-25) into `bindings/rust`,
  with provenance recorded in `bindings/rust/UPSTREAM.md`.
- It scopes the import to the core client and tester crates plus minimal C ABI simulator
  support. Section 2 discusses what else should come in.
- It also touches the C client (`bindings/c/fdb_c.h`, `bindings/c/fdb_c.cpp`) so that several
  Rust libraries loaded in one process can share the selected API version.
- Rust is opt-in in CMake (`BUILD_RUST_BINDING=OFF` by default). The only Rust CI that runs
  on the PR is a new GitHub Actions workflow (`.github/workflows/rust.yml`).

## Proposed sequence

1. **Converge foundationdb-rs.** Land Trevor's fixes (#527, #530, #531, #534, #536, #539,
   #540, #542, #547) and pending work in foundationdb-rs, so that its main branch matches
   what #14187 corrects.
2. **Cut a release of foundationdb-rs.** Current users get the fixes before anything moves.
3. **Import from that tag.** I open the import PR upstream from the release tag, reusing the
   CMake and bindingtester wiring from #14187.
4. **First release from upstream.**
5. **New features upstream.** FDB 8.0 support (API 800, which foundationdb-rs does not have
   yet), native CDC, and anything else, developed directly in apple/foundationdb.

## Questions for the FoundationDB maintainers

### 1. Shape of the import

1. **In-tree or separate repo?** Options: in-tree `bindings/rust` (like Go, Java, Python,
   and #14187), the Swift hybrid model (`FoundationDB/fdb-swift-bindings`, pulled into the
   build through `FetchContent`), or a plain separate repo in the FoundationDB org (like
   the record layer and the operator). Which one does the core team prefer for Rust?
   Our proposal: in-tree, so that C API changes, the bindingtester and simulation evolve
   together with the binding.
2. **Git history.** Squashed snapshot plus `UPSTREAM.md` (as #14187 does, and as the
   original 2017 import of Go/Java/Python did), or a subtree merge preserving about 1200
   commits from about 40 contributors? Is the provenance file enough attribution?
3. **Process.** `CONTRIBUTING.md` asks for a forum discussion before large changes, and
   issue #200 (2018) was redirected to the forums for exactly this question. Should we
   open a forum thread before the import PR?
4. **C client change.** Should the `fdb_c.h`/`fdb_c.cpp` change from #14187 land as its
   own PR with its own review, ahead of the Rust import?
5. **API parity.** Is reaching API 800 (and native CDC) a condition for merging, or a
   follow-up? Our proposal: a follow-up, and the first feature developed in-tree.
6. **Existing Rust in the tree.** Should `bindings/c/test/workloads/RustWorkload` be
   replaced by `foundationdb-simulation`? Its own code says it should be.

### 2. Scope

Proposed split:

| Upstream (`bindings/rust`) | Outside (`contrib/` or a FoundationDB org repo) |
|---|---|
| `foundationdb-sys`, `foundationdb-gen`, `foundationdb-macros` | recipes (leader election, ranked register) |
| `foundationdb-tuple`, `foundationdb` (core client, extensions included, see question 7) | `foundationdb-recipes-simulation` |
| bindingtester | `foundationdb-profiling` |
| `foundationdb-simulation`, `foundationdb-simulation-tracing` | |

Simulation is one of the strongest arguments for the Rust bindings: Rust application code
runs inside the FDB deterministic simulator, with the same fault injection as fdbserver
itself. We propose to import the simulation crates fully, extending the C ABI support
prototyped in #14187.

7. **Beyond the spec.** The Rust client has APIs other bindings do not have: runner hooks,
   typed retry policies, client-side transaction budgets, metrics, a pluggable clock.
   Apart from the recipes, these extensions cover what the FoundationDB Record Layer has
   to build on top of the Java binding: StoreTimer-style metrics, transaction lifecycle
   hooks (as with its `TransactionListener` and commit checks) and client-side limits on
   what a transaction reads and writes (as with its scan limiters). The retry policy is a
   deliberate exception: the Record Layer runs its own backoff loop, while the Rust runner
   keeps `fdb_transaction_on_error` as the single retry governor. The pluggable clock is different: it exists so that client code
   stays deterministic under the FDB simulator (time comes from the simulation, not the
   wall clock), because simulation is how Clever Cloud tests its FDB-based systems.
   #14187 scoped them out, pending this discussion. Are such extensions acceptable in an official binding? Our proposal:
   keep them in the core crate.
8. **No break for current users.** If the extensions are dropped, users moving from 0.x to
   the first upstream release lose APIs they use today. Keeping them makes the first
   upstream release a continuation of foundationdb-rs rather than a breaking change.
9. **Multi-version support.** One foundationdb-rs build targets any API version from 510
   to 740 through cargo features. Other in-tree bindings carry only the API version of
   their release branch. Our proposal: keep the feature model, since it is what lets one
   crate built from `main` serve every supported server version (see question 25).
   Dropping very old API versions can be discussed separately.

### 3. Ownership and access

10. **Committer status.** Can I become an external committer, scoped to `bindings/rust`
    (there is no CODEOWNERS file on `main` today)? At minimum, can my PRs trigger CI
    automatically?
11. **CI visibility.** What access to CI logs would a Rust maintainer get to debug
    failures?
12. **Reviewers.** Who on the core team reviews Rust changes, and who is the second
    maintainer?
13. **Commitment.** The Node.js bindings were removed in 2018 because nobody maintained
    them. What does the project expect from Rust maintainers to avoid the same outcome?
14. **C API changes.** When a core change breaks the Rust binding, who fixes it, and does
    a broken Rust build block core PRs?
15. **Source of truth.** After the import, what happens to `foundationdb-rs/foundationdb-rs`
    (archived, or read-only mirror), and where do its 39 open issues and 22 open PRs go?
    Should recipes and profiling go to `contrib/` in apple/foundationdb, or to a separate
    repo in the FoundationDB org?

### 4. CI and testing

16. **GitHub Actions.** Is it fine to keep a path-filtered `rust.yml` in-tree (check,
    clippy, fmt, unit tests)? It is the only place Rust is built today, and it costs core
    PRs nothing.
17. **Build image.** `fdb-build-support` has no Rust toolchain: `rockylinux9` never had
    one, and the 2022 `centos7` install (Rust 1.59) was removed in June 2026. Can we add
    rustup at the MSRV, so CodeBuild builds Rust with `BUILD_RUST_BINDING=ON`?
18. **Crate dependencies.** Can CI fetch crates from the network, or must dependencies be
    vendored?
19. **Joshua.** The bindingtester runs at scale under Joshua, not in ctest. Can Rust be
    added to the Joshua bindingtester package, and who triages Rust failures there?
20. **Campaigns we run today.** The hourly bindingtester, the FDB version matrix, the
    nightly/beta toolchains and the simulation campaigns are not part of #14187. Is it
    fine to keep running them from the `foundationdb-rs` org against upstream
    `bindings/rust` until Joshua covers Rust?
21. **Simulation ABI.** fdbserver 7.4.3 to 7.4.5 changed the C workload ABI without a
    version gate (crash on load). Can the C workload API carry a version field?

### 5. Versioning and releases

22. **Version numbers.** Every official binding publishes in lockstep with FDB (Python,
    Java and Ruby are at 8.0.0). foundationdb-rs uses its own semver (0.11). Does the
    `foundationdb` crate jump to 8.0.0, or keep independent semver like the record layer
    and the operator? Our proposal: independent semver. Rust API changes move faster
    than FDB minor versions, and the Go binding shows the cost of having no usable
    versions (issues #3338 and #4431).
23. **Publishing.** Who runs `cargo publish`, and where does the crates.io token live?
    Does Apple's release pipeline publish crates, or do the maintainers?
24. **crates.io ownership.** Today the owners are individuals, and the list needs a
    cleanup: on `foundationdb`, `foundationdb-sys` and `foundationdb-gen`, three of the four
    owners have been inactive for years but can still publish. We will clean this up
    before the import either way. After that, should a crates.io team controlled by the
    project be added as owner (with Rust maintainers as members)?
25. **Release branches.** Python, Java and Go ship one release line per FDB branch because
    each hardcodes its branch's header version, and `libfdb_c` rejects a newer header.
    The Rust crate selects the header version through a cargo feature, so one crate built
    from `main` already serves 7.3, 7.4 and 8.0 users. Our proposal: Rust lives on `main`
    only (as Swift does), is not maintained on release branches, ships patch releases
    independently of server releases, and proves compatibility with a CI matrix against
    supported server versions. Binding backports are rare anyway (11 binding commits on
    `release-7.4` since its cut, mostly build fixes). When FDB cuts `release-X.Y`, we add
    its header and an `fdb-X_Y` feature on `main`. Is that acceptable?
26. **Release notes and docs.** Release notes in the Sphinx release notes, or our
    generated `CHANGELOG.md` files? API docs on docs.rs only, or an `api-rust.rst` page?

### 6. Licensing

27. **License notice.** The crates are `MIT OR Apache-2.0`, so they can be taken under
    Apache-2.0 without contributor consent. Keep the dual notice in-tree (as #14187 does),
    or Apache-2.0 only?
28. **File headers.** Add the standard "Apple Inc. and the FoundationDB project authors"
    header to every file, or keep the existing foundationdb-rs headers on imported files?

## Before the import (foundationdb-rs side)

Actions on our side, not questions:

- [ ] Review and land Trevor's open PRs (3 of them are breaking: #527, #534, #542).
- [ ] Unblock release-plz: every run since 2026-09-14 fails because the
      `foundationdb-profiling-v0.1.0` tag exists but the crate was never published.
- [ ] Decide whether `foundationdb-simulation-tracing` gets published in the last
      foundationdb-rs release (today the next release would publish it).
- [ ] Move recipes out of the core crate (they are enabled by default today), so the
      import does not have to carry them.
- [ ] Clean up crates.io owners: `foundationdb`, `foundationdb-sys` and `foundationdb-gen`
      are also owned by three inactive accounts (last commits in 2018, 2019 and 2022).
- [ ] Fix `LICENSE-MIT` (copied boilerplate naming "The trust-dns Developers" and
      "Google LLC"), the deprecated `MIT/Apache-2.0` SPDX form, and stale Clikengo URLs
      in file headers.
- [ ] Diff #14187's `bindings/rust` against foundationdb-rs `main` once Trevor's PRs land,
      to catch fixes that only exist in the import PR.
- [ ] Cut the release, then open the import PR from its tag.

## Appendix: how the other bindings are handled

| | Go | Java | Python | Swift | Rust today |
|---|---|---|---|---|---|
| Location | in-tree | in-tree | in-tree | separate org repo | separate org |
| Release branches | yes | yes | yes | no (`main` only) | n/a (one branch) |
| Version | FDB version (pseudo-versions) | FDB version | FDB version | no releases | own semver |
| Generated options | committed, staleness ctest | generated at build | generated at build | committed | generated at build |
| Bindingtester | Joshua | Joshua | Joshua (reference) | Joshua | GitHub Actions, hourly |
| Built in CodeBuild | yes | yes | yes | yes (Swift 6.1) | no |
