# Plan: remove the metadata dependency

## Intent

The repository decodes chain data through subxt, and subxt decodes it using the chain's runtime metadata. That coupling costs us twice: the checked-in metadata file needs constant refreshing, and a newer metadata version forces a newer subxt whose breaking changes churn our code. We want to read the chain without metadata. subxt stays as the transport for now; removing it entirely is a later, self-contained follow-on. What we remove here is every use of metadata to *decode*: the codegen macro, the `.scale` asset, the typed storage/runtime-API addresses, the metadata-driven event and extrinsic decoding, and the dynamic `scale-value` path.

**Done when:** the tools build and run across runtime upgrades with no metadata present — the `#[subxt::subxt]` macro is gone, `essentials/assets/polkadot_metadata.scale` is deleted, the CI refresh job is removed, and output is unchanged.

## How we verify (read this before starting tasks)

A step is not done because it compiles and a unit test passes — that proves the decoder, not the tool (a lesson that cost two resets). The gate is a **shadow decode** inside the running binary: for each migrated read, decode both the old (metadata) and new (metadata-free) value at the same block hash and assert they are equal.

- Behind the `--shadow-decode-without-metadata` flag.
- **Return the old value while shadowing** — turning the flag on only observes, never changes behaviour. Flip a read to the new value once it has shadowed clean over enough blocks.
- Abort loudly on any mismatch, reporting `(block, read, old, new)` — a mismatch is a decoder defect, not something to tally and move past.
- Compare collections as sets keyed by identity (e.g. candidate events by `candidate_hash`), not by order.
- Drive it live (continuous, uncurated) and over `--historical --from A --to B` ranges that contain the activity (backing/inclusion, a real dispute, on-demand, `--channels`).
- The scaffold is temporary: it needs both paths present, so it is gone once metadata is deleted (the final check is then a plain offline output diff).

## Principles

- **Decode only the fields we need, positionally.** Read the leading fields we use; ignore the rest. Tolerates fields appended in an upgrade; fails loud on a field inserted-before or retyped — never a silently wrong value.
- **Prefer runtime calls over raw storage.** They are stable and versioned, and reading candidate/dispute activity through them avoids `System.Events` — the one structure SCALE cannot traverse without metadata.
- **Keep the seam.** `RequestExecutor` / `ApiClient` signatures stay fixed; everything behind them changes. Confines blast radius so a shadow mismatch points at the one read that changed.
- **Keep decoders standalone.** Write each decode as free functions in a decode module, not inline in the executor's match arms — so a later transport refactor is a deletion, not a rewrite.
- **Detect API versions without metadata.** `state_getRuntimeVersion` lists `(api_id, version)` pairs; find `ParachainHost` by the well-known id `af2c0297a23e6d3d` (sp_api hashes the full trait path, so it is *not* `blake2_256("ParachainHost")`). Polkadot is on v16.

## Read → new source (reference)

All chain-data decoding lives in `essentials/src/api/` plus two `field_values()` calls in `chain_events.rs`. `whois`, `kvdb`, `jaeger` do not decode chain data this way.

| Read | metadata used today via | new source |
|---|---|---|
| candidate events (backed/included/timed-out) | `System.Events` decode | `ParachainHost_candidate_events` |
| disputes (initiated/concluded) | `System.Events` + inherent | `ParachainHost_disputes` (shape reconciliation) |
| occupied cores | dynamic runtime API | `state_call availability_cores` + positional |
| claim queue | typed runtime API | `state_call claim_queue` + positional |
| backing/validator groups | `ParaScheduler.ValidatorGroups` storage | `ParachainHost_validator_groups` |
| session index | `Session.CurrentIndex` storage | `ParachainHost_session_index_for_child` |
| session validators / account keys | `ParaSessionInfo.AccountKeys` storage | `ParachainHost_session_info` |
| babe randomness / authorities / slot | `Babe.*` storage + `System.Digest` | `BabeApi_current_epoch` |
| ParaInherent bitfields | `block.extrinsics().field_values()` | `chain_getBlock` + positional |
| block timestamp | `Timestamp.Now` typed storage | manual `twox128` key + `u64` |
| HRMP channels (`--channels`) | `Hrmp.*` typed storage | manual key + positional |
| host configuration | `Configuration.ActiveConfig` storage | manual key + positional (fragile) |
| session next/queued keys, key owner | `Session.*` typed storage | manual key + positional |
| head / block number | subxt blocks API | `chain_getHeader` |
| on-demand orders | `System.Events` decode | OPEN — no runtime call found |

## Task list

Work top to bottom; each is one reviewable change, green build, zero shadow mismatches. Runtime calls come first — they are the most stable and they kill the `System.Events` dependency.

- [x] 1. Build the shadow-decode harness: `--shadow-decode-without-metadata` flag, per-read wrapper (decode both, return old, compare), abort loudly on any mismatch. (`api/shadow.rs`: `compare` / `compare_set` (set keyed by identity) → `process::exit(1)` on mismatch. Flag wired through `RequestExecutor` (`shadow_enabled()`) → `ApiClient`.)
- [x] 2. Candidate events → `ParachainHost_candidate_events` (shadowing; `ParaInclusion` scrape stays primary — see below). See note 2. (Metadata-free `decode_candidate_events` (`api/decode.rs`, positional, fail-loud, unit-tested) + `ApiClient::get_candidate_events` via `state_call`; collector shadow-compares it against the scraped `ParaInclusion` events as a set keyed by `(candidate_hash, event_type)`, returning the scraped ones. **Shadow verified live** against Polkadot: clean over 732 heads / 809 backed + 781 included, zero mismatches. The `ParaInclusion` scrape stays the primary source and nothing is removed here — the metadata-free path only shadows. Switching it to primary (old path then shadowing in reverse for a while) is the final-final step; the old decode is deleted only when metadata itself goes (task 17).)
- [x] 3. Disputes → `ParachainHost_disputes`; map `DisputeInfo` first (note 3), then stop scraping `ParasDisputes`. Metadata-free `decode_disputes` (`api/decode.rs`, positional, fail-loud, unit-tested) decodes `Vec<(SessionIndex, CandidateHash, DisputeState)>`; the two validator bitsets are read as set-bit indices and `start` / `concluded_at` are kept. `ApiClient::get_disputes` via `state_call("ParachainHost_disputes")`, wired through the executor. The collector shadow-compares it against the scraped `ParasDisputes` events per block: the runtime call returns the rolling window, sliced by relay block number to the per-block deltas — `start == block_number` shadows `DisputeInitiated` (compared as a set of candidate hashes), `concluded_at == Some(block_number)` shadows `DisputeConcluded` (set keyed by candidate hash, value carries the outcome). **Mapping decisions (note 3):** outcome derived as `validators_for.len() >= validators_against.len() → Valid else Invalid` (the prevailing supermajority side); `TimedOut` never concludes so it appears on neither path. `initiator_indices` is *not* shadow-compared — the runtime's `validators_against` is the rolling set of all against-voters, not the per-inherent `Invalid` statements the tracker records, so they legitimately diverge; reconcile that only when switching this read to primary. The scraped `ParasDisputes` path stays primary and nothing is removed here. **Verified:** live against Polkadot, the disputes shadow ran clean over blocks with zero mismatches/aborts (plumbing: `state_call` → `decode_disputes` → per-block set comparison). No dispute occurred in that window, so the populated-dispute path (`start` / `concluded_at` slicing and the outcome derivation) is unexercised — re-check it over a `--historical` range containing a real dispute before switching this read to primary.
- [x] 4. Occupied cores → `state_call availability_cores` + positional (free/scheduled/occupied). Metadata-free `decode_availability_cores` (`api/decode.rs`, positional, fail-loud, unit-tested) decodes `Vec<CoreState>` by variant tag (`0=Occupied / 1=Scheduled / 2=Free`), advancing past the discarded `OccupiedCore` payload via codec-derived `Raw*` structs (the `availability` bitset consumed by a manual `SkipBitVec`, `candidate_descriptor` pinned to the 292-byte window). Order preserved — cores are addressed by index. The shadow lives inside `ApiClient::get_occupied_cores`: the existing scale-value (dynamic runtime API) decode stays primary; when `--shadow-decode-without-metadata` is set it also fetches via `state_call("ParachainHost_availability_cores")`, decodes positionally, and `shadow::compare`s the two Vecs (order-sensitive). **Shadow verified live** against Polkadot: clean over 16 heads (`write_occupied_cores` runs every head), zero mismatches — all three variants and the occupied-core skip path exercised, since cores are always populated. Switching to primary and deleting the dynamic path happens with task 17.
- [x] 5. Claim queue → `state_call claim_queue` + positional. Metadata-free `decode_claim_queue` (`api/decode.rs`, unit-tested): the runtime returns `BTreeMap<CoreIndex, VecDeque<Id>>`, whose wire format (compact entry count + key-sorted `(u32, Vec<u32>)` pairs) matches `ClaimQueue = Vec<(u32, Vec<u32>)>` directly, so codec `decode_all`s it whole and fails loud on trailing bytes — no payload to skip. Shadow lives inside `ApiClient::get_claim_queue`: the typed metadata path stays primary; under `--shadow-decode-without-metadata` it also fetches via `state_call("ParachainHost_claim_queue")` and `shadow::compare`s the two Vecs (both key-sorted, so order matches). **Shadow verified live** against Polkadot: clean over 15 heads (`write_core_assignments` → `get_claim_queue` runs every head), zero mismatches. Switching to primary and deleting the typed path happens with task 17.
- [ ] 6. Validator groups → `ParachainHost_validator_groups`.
- [ ] 7. Session index and validators → `session_index_for_child`, `session_info`.
- [ ] 8. Babe values → `BabeApi_current_epoch` (randomness, authorities, slot).
- [ ] 9. Block timestamp → `twox128("Timestamp") ++ twox128("Now")`, decode `u64` (ms). See note 9.
- [ ] 10. HRMP channels → manual keys for the three maps + positional.
- [ ] 11. Session next/queued keys, key owner → manual keys + positional (revisit after task 7).
- [ ] 12. Host configuration → manual key + positional; fragile, fail loud (note 12).
- [ ] 13. ParaInherent bitfields → `chain_getBlock` + positional.
- [ ] 14. Heads / block numbers → `chain_getHeader`; confirm header subscriptions are metadata-free.
- [ ] 15. On-demand orders → find an `OnDemandAssignmentProvider` storage source, else keep a narrow event decoder or defer. Gates deleting `get_events`.
- [ ] 16. Own the types: replace metadata-derived types (`SessionKeys`, `ValidatorIndex`, `HrmpChannel`, `types.rs` aliases…) with owned structs deriving codec; hash type via `primitive-types` `H256`.
- [ ] 17. Delete metadata: remove the `#[subxt::subxt]` macro and `metadata.rs`, delete `polkadot_metadata.scale`, drop the CI refresh job. Final offline output diff must be empty.

### Optional follow-ups (after task 17)

- [ ] 18. Collapse the executor into a `ChainReader` trait: introduce the trait as a thin facade over the executor, repoint the collector/trackers, then delete the `Request`/`Response` enums, `match_request`, `wrap_backend_call!`, and (if prioritisation is unused — verify first) the `priority-channel` dep. Verify multi-node/retry by hand; an output diff won't catch it. Do this *after* metadata removal, never interleaved. Shape proven in `~/code/parachain-tracer` (`tracer-chain`).
- [ ] 19. Remove subxt entirely: swap the transport for `subxt-rpcs` or an owned client. The PoC ran on `jsonrpsee` alone against live Polkadot, so the surface (`state_call`, `state_getStorage`, `chain_getBlockHash`, `chain_getHeader`, `chain_subscribeNewHeads`) is small and stable.

## Notes

- **2 — candidate events.** Result is a `Vec` of variants `0=Backed / 1=Included / 2=TimedOut`, each `(CandidateReceipt, HeadData, CoreIndex[, GroupIndex])` — `GroupIndex` absent only for `TimedOut`. `CandidateReceipt` is a fixed 324-byte window (292-byte descriptor + 32-byte commitments hash): read `para_id` (`u32` LE) and `relay_parent` (32 bytes) from its front, compute the candidate hash as `blake2_256` of the whole window, read `CoreIndex` as the event's own field, skip `HeadData` by its compact length. Re-confirmed against live Polkadot in the PoC.
- **3 — disputes.** The runtime `disputes()` result may not map one-to-one onto the tracker's `DisputeInfo` (initiator indices, session, outcome). Resolve the mapping before coding; shadow-decode over a range with a real dispute confirms it.
- **9 / storage keys.** Use `sp-crypto-hashing` for `twox_128` / `twox_64` / `blake2_128` — small, stable, metadata-free (PoC-verified for the timestamp key).
- **12 — host configuration.** A reordered struct read by position — the lowest-confidence decode. Length-check and fail loud on any mismatch.
- **Positional fragility in general.** Every decoder must error, not guess, on an unrecognized layout; shadow-decode against the old path is what catches a silent mistake the decoder itself accepts.
- **Verification needs a node.** Shadow-decode and the historical ranges require an archive/full node; budget for that being Linux/CI-only. Shadow mode doubles RPC load — it is off by default.
