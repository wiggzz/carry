# Full preparation failure: 35512750813

Source: `b07716895485d9e91614516177ad4c0c9b44a98d`. Artifact: `swebench-prepare-long-50-carry-35512750813-1-attempt-1` (ID `10607580591`). The result archive and controller diagnostics were retained; exact-worker termination and cleanup passed. No model calls occurred.

## Reconciled outcome

The attempt checkpoint contains all 50 frozen task IDs: 34 published evaluator/agent pairs, 15 readiness failures, and one environment-blocked task. The completed-pair checkpoint contains exactly 34 entries. No complete catalog was published.

- Twelve historical SymPy tasks ran `bin/test -C --verbose --timeout 15 --split 1/500`; each output reports zero tests. Exit zero is not readiness success.
- Three Sphinx tasks (`7440`, `7590`, `8056`) failed importing `sphinx.testing.fixtures` because `roman` was absent.
- `matplotlib__matplotlib-24627` was blocked by its shared environment. The classic Conda solver was killed with exit 137 at 13:38:57 UTC. Kernel OOM evidence was not captured, so exit 137 alone does not establish OOM.

`matplotlib__matplotlib-21568` and `scikit-learn__scikit-learn-25102` passed the existing readiness gate and were published: the earlier Qhull and legacy-pip repairs progressed successfully on this run.

## Timing and resource evidence

- Workflow creation through terminal update: 5,632 seconds (about 94 minutes).
- Preparation checkpoint elapsed: 5,456.497736 seconds (about 91 minutes).
- Dependency-build stage: 3,222.205588 seconds (about 54 minutes).
- Remaining preparation: 2,234.292148 seconds (about 37 minutes), including readiness and publication.
- Retained controller evidence: 90 preparation heartbeats and 171 polls; final outcome `archive-received` and observed worker state `terminated`.

During the Matplotlib solve, available host memory fell from 21.76 GiB at 13:36:31 UTC to 13.97 GiB at 13:37:31 and 3.35 GiB at 13:38:31, then recovered to 28.29 GiB at 13:39:31. Load was approximately one before the kill. This supports investigating solver memory growth; it does not prove a kernel OOM kill or justify blaming the five-build concurrency limit. Serializing alone is unlikely to address a mostly single-active solver phase.

## Repair acceptance

Keep the original full-50 selection, evaluator commands, dependency requirements, gold separation, readiness requirement, and existing worker/controller deadlines. Reproduce the empty historical SymPy shard with public source; choose bounded public tests that actually execute. Scope dependency/solver compatibility repairs to validated upstream recipes, record provenance, and test the real command behavior where possible.

The shared compatibility module participates in every prepared-image recipe hash. Changing it invalidates all prior ready-pair cache identities, including the 34 pairs from this attempt. Those published artifacts remain intact but must not be silently reused under a changed identity. Do not describe retained artifacts as cache hits unless the current policy validates them.

The follow-up branch incorporates merged PR #118 before combined testing. This failure run itself predates #118 and is not evidence for that code.

## Locally reproduced repairs

- SymPy: historical runners split files, so the first of 500 shards can be empty. Select the fixed public module `sympy/core/tests/test_basic.py`, present with tests at all 15 cohort SymPy base commits. Genuine Python 3.9 runner/SWE-bench parser probes produced 15, 20, and 22 parsed passing tests at three representative commits; the old probe produced zero, zero, and nine. Preserve the 15-second per-test bound, 180-second outer timeout, and nonzero parsed-test requirement. Selection never reads gold/test patches or evaluator selectors.
- Sphinx: add only `roman==3.3` with `--no-deps`, for the three failing task IDs under exact original setup-hash guards. Real Python 3.9 probes reproduced the missing import, then passed eight public tests at each base commit after adding that package. Existing dependencies and evaluator instructions are unchanged.
- Matplotlib remains unresolved. A checksum-verified micromamba 2.3.3 experiment retained the YAML/channel order and applied the already-required Python 3.11 constraint upfront, but the local 1-GiB safety cap was exhausted while parsing package metadata. This is neither a successful solve nor evidence that the dependency set is unsatisfiable. The local host has only 3.7 GiB total memory, so increasing its cap would endanger unrelated services. A bounded, no-secret hosted diagnostic is required before changing production solver behavior or spending another full-50 preparation attempt.

## Hosted solver diagnostic and parser compatibility

Diagnostic `35519650923` at `9840279c95f22316a43e579f8363b663255fe043` retained its evidence but failed: micromamba exited 1 after 35.575 seconds, with peak RSS 1,562,960 KiB. Neither its 600-second deadline nor an observed memory-limit failure explains this result. Its structured error was `solver_problems: ["unsupported request"]`. Normal CI `35519634829` separately passed both test and infrastructure jobs; diagnostic success would not substitute for those checks.

A bounded offline reproduction using the real pinned micromamba binary and the unmodified Conda 23.11.0 MatchSpec implementation isolated one of 47 declared requests: `nbconvert[execute]!=6.0.0,!=6.0.1`. Micromamba treats the interior brackets as part of the package name, whereas historical Conda ignores that bare bracket attribute. The canonical request `nbconvert[version='!=6.0.0,!=6.0.1']` compares equal under historical Conda. Seven singleton-version fixtures prove both excluded versions remain excluded. Combined synthetic requests reproduced the hosted error byte-for-byte and succeeded after only this canonicalization; these deliberately synthetic packages are parser evidence, not a real dependency solve.

The next solver-only diagnostic preserves the original YAML unchanged and separately records the effective YAML and SHA-256, with only that exact request canonicalized under the original fixture's hash guard. Channels, Python constraint, solver binary, resource limits, and all declared dependencies remain unchanged. The retained Conda semantics never enforced a pip extra; this must not be described as adding one. Production preparation is unchanged. Real package resolution, pip installation, image build, and readiness remain unverified until independently exercised. This diagnostic defect does not establish the cause of the earlier classic-Conda exit 137.

The canonicalized hosted run `35520935342` at `eeb8a4a175c3c156af30d033210e58e759440c97` then produced a real successful dry-run: solver exit 0, 29.447 seconds, peak RSS 1,593,496 KiB, and 318 planned packages. The workflow still failed because the diagnostic's numeric-only validator rejected the valid `python-dateutil` version `2.9.0.post0`. A failing-first regression now supports numeric post-releases without weakening version floors or exclusions. Replaying the retained plan through both the corrected validator and upstream Conda 23.11.0 MatchSpec validates every one of the 47 original requests; no new solve was required for that validation repair. The raw plan SHA-256 is `86bb465df88abd1bd010c571dcd50b5902f4e3d4905c878bf25abbd3a4192c86`. Its Python is 3.11.16; nbconvert 7.17.1, nbclient 0.11.0, and jupyter_client 8.10.0 are present. This establishes bounded real dependency resolution, not installed-package, image-build, or readiness success. The original failed workflow/result remains unchanged, with the local replay recorded separately.

## Actual image transaction failure and bounded allocator mitigation

Actual-image diagnostic `35522520181` at `5fb02700c699bd4b870526e138e99c1d45175898` built the base and resolved the environment inside the actual Ubuntu 22.04 container, then failed during the 318-package installation transaction: `critical libmamba callback invocation failed : Resource temporarily unavailable`. The supervisor took 191.88 seconds, retained untruncated hash-verified evidence, and verified container cleanup. The instance image and readiness tests were not reached. Regular CI `35522500123` separately passed at that SHA.

A checksum-identical real micromamba reproduction using 40 tiny **synthetic offline packages** under a 512-MiB address-space limit reproduced the exact message. Traces show thread-stack allocation returning ENOMEM after glibc arena reservations fill virtual address space; RSS was low. Three default runs failed and three runs with only `MALLOC_ARENA_MAX=2` installed all 40 verified payloads. An independent parent repetition confirmed that split; one failing default process emitted the same callback error and then segfaulted, rather than returning the fixture's expected exit 1. That original assertion failure and all raw results were retained, not rerun away. This proves a local mechanism and mitigation, but the hosted artifact lacks the syscall/limit evidence needed for conclusive hosted attribution; it is not OOM-killer evidence.

Scope `MALLOC_ARENA_MAX=2` only to micromamba and its descendants. Keep the YAML, binary, channels, version constraints, `--no-rc --no-env`, and all address-space, CPU, Docker and wall-clock limits unchanged. The emitted-shell regression proves the solver receives the bounded allocator setting while subsequent official pip commands retain their original environment. A fresh actual-image diagnostic is still required; neither synthetic installation nor the successful dependency solve proves full preparation readiness.
