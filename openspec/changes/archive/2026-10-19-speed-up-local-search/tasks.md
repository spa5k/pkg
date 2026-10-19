# Tasks

- [x] P1. Measure release search against a complete large local snapshot.
- [x] P2. Borrow native package text and allocate result rows after matching.
- [x] P3. Reduce lowercase copies and preserve Unicode matching.
- [x] P4. Add a repeatable CLI speed comparison with output checks.
- [x] V1. Run required checks on macOS and Linux and record the results.

- [x] V2. Measure all CLI groups, profile scales, catalog scales, resources, and catalog generation on macOS and Linux.

## Immutable query facts follow-up

- [x] M3. Compare cached term lengths, library intent, and fuzzy bounds.
- [x] V3. Verify exact output, Unicode rules, all search sources, and resources.
- [x] D3. Keep the change only if complete search timings justify it.
- [x] R3. Record every sample and separate unchanged compiler settings.

Use the accepted default-profile release as the baseline on both hosts.
Do not change matching rules or SQLite integrity checks.

Reject cached query facts. Timings are small and mixed.
The accepted matcher is restored exactly.

## Search spool indexes follow-up

- [x] M4. Compare only the indexes used by each output mode.
- [x] V4. Verify full output, limits, source groups, fuzzy fallback, and resources.
- [x] D4. Select only from complete timings on both hosts.
- [x] R4. Record normal and million-row catalog samples.

Keep the same result table, queries, ordering, and bounded storage.
Use the accepted default-profile release as the separate baseline.

Keep output-specific indexes. Full-output gains pass on both hosts.
Short-query gains remain unproven.


## Search spool prepared insert follow-up

- [x] M5. Compare reuse of the fixed result insert statement with the accepted spool client.
- [x] V5. Verify exact full output, small queries, limits, groups, fuzzy fallback, and memory.
- [x] D5. Select only from repeated end-to-end results on both hosts.
- [x] R5. Record the trial source, complete samples, and any rejection.

Keep SQL, parameters, ordering, row counts, and bounded SQLite storage unchanged.
The candidate only reuses the compiled insert statement on the same connection.
Keep the accepted binaries frozen. Do not compile beside a timed workload.

Keep the prepared INSERT. Both full Rust and native lifecycle checks pass.
Dense full output improves on both hosts. Retain initial negative samples.


## Search output row borrowing follow-up

- [x] M6. Compare borrowed SQLite output text with the accepted prepared-insert client.
- [x] V6. Check full output, Unicode, fuzzy fallback, grouped limits, and resources.
- [x] D6. Select only from repeated complete results on both hosts.
- [x] R6. Keep the exact source, all samples, and any rejection.

Change only the body of visit_rows. Keep the SQLite text and UTF-8 checks.
The callback uses the text before the row advances. Do not change query SQL,
ordering, parsing, result counts, integrity checks, or bounded temp-file storage.
Wait for the active strip and checksum jobs and evidence capture before building.


## Unlimited JSON result sorting follow-up

- [x] M7. Compare one SQLite output sort with per-row order-index updates for unlimited JSON.
- [x] V7. Check exact ranks, Unicode, fuzzy fallback, full output, grouped limits, and resources.
- [x] D7. Select only from complete repeated results and full checks on both hosts.
- [x] R7. Keep the exact source, every sample, any negative result, and decision.

Change only SearchSpool::new. Keep all grouped-text and limited-output indexes.
Keep SQLite temp_store=FILE, cache size, schema, insert statement, and output SQL.
The unlimited JSON output query already orders by rank and id.
Wait for the borrowed-text timing and evidence capture before building.

Reject output-row borrowing. Gains are small and mixed.
Both exact-output matrices pass. Accepted source remains selected.

Reject unlimited JSON index removal. Isolated Linux dense output is17.1%slower.
Keep the accepted output-specific indexes and prepared INSERT.


## Native source ID buffer follow-up

- [x] M8. Compare reuse of the native source ID buffer before matching.
- [x] V8. Check all output, Unicode, fuzzy fallback, source groups, and resources.
- [x] D8. Select only after complete repeated end-to-end results on both hosts.
- [x] R8. Keep exact source, every sample, verification, and any rejection.

Change only search_local_native. Reuse the ID buffer for nonmatching rows.
Transfer the buffer into each matched owned result. Keep source validation,
SQLite reads, ranking, matching, and output fields unchanged.
Use the accepted prepared-insert binaries. Keep release settings unchanged.
Start only after final matrix, native phase diagnosis, and evidence capture.

Keep the native source ID buffer. Both hosts pass all exact outputs and full checks.
Retain the slightly slower normal Mac dense-output median.

## ASCII uppercase folding follow-up

- [x] M9. Compare the ASCII lowercase operation with the accepted ID-buffer client.
- [x] V9. Check Unicode, ranking, all output modes, fuzzy fallback, and resources.
- [x] D9. Select only after complete repeated results on both hosts.
- [x] R9. Keep exact source, all samples, verification, and any rejection.

Change only the lowercase helper body. Keep borrowed lowercase ASCII text.
Use ASCII conversion only after the full text passes is_ascii.
Keep the original Unicode conversion for every non-ASCII string.
Keep ranking rules, cache checks, compiler settings, and resource bounds.
Restore accepted source before timing. Do not build beside active timing.

## Accepted search CPU profile

- [x] M10. Profile instruction costs for real accepted search commands.
- [x] V10. Require identical output bytes in profiled and ordinary calls.
- [x] D10. Use the profile to choose the next change or record no change.
- [x] R10. Save tool versions, exact commands, profiles, output hashes, and limits.

Use a separate Linux host while full setup timing runs elsewhere.
Use the exact selected release and a complete generated local catalog.
Keep runtime lookup disabled through an empty PATH. Preserve real SQLite work.
Callgrind instruction shares are diagnostic. They are not wall-time percentages.
Do not claim a speed gain from instrumented durations.


Reject the ASCII lowercase trial. Keep all slower cases and exact output evidence.
The accepted-client instruction profile passes all five exact output comparisons.

## Bounded ASCII typo-distance follow-up

- [x] M11. Compare fixed-row ASCII OSA distance with the selected ID-buffer client.
- [x] V11. Compare all short ASCII strings with the reference algorithm and check Unicode bounds.
- [x] D11. Select only after repeated end-to-end results on both hosts.
- [x] R11. Keep exact source, every sample, resource data, checks, and any rejection.

Change only edit_distance and its behavioral checks in matching.rs.
Keep non-ASCII calls on the current reference algorithm.
Keep the 50-character right-input limit, length bound, adjacent transpositions,
ranking rules, compiler settings, and output bytes.
Use three fixed rows and swap references. Add no unsafe code or dependency.
Restore accepted source before timing. Do not build beside active timing.


## Repeated fuzzy candidates follow-up

- [x] M12. Compare removal of duplicate fuzzy inputs with the selected ID-buffer client.
- [x] V12. Check all exact outputs, Unicode, typo ranking, and resource bounds.
- [x] D12. Select only after repeated end-to-end results on both hosts.
- [x] R12. Keep source, all samples, resources, checks, and any rejection.

Change only typo_rank. The minimum distance is unchanged when duplicate names
and words equal to their full name are checked once.
Keep the reference edit-distance algorithm and all matching rules unchanged.
Run separately from the fixed-row OSA trial, with the same selected baseline.
Restore accepted source before timing. Build only after the earlier trial ends.


## Complete real-catalog search follow-up

- [x] M13. Compare origin and selected local search against the real pinned native catalog.
- [x] V13. Require the complete native row hash, exact outputs, unchanged database, and empty profile.
- [x] R13. Keep every sample, source identity, resource pass, and any trial comparison.

Prepare once in a private home. Compare the same complete snapshot on both clients.
Use the established pinned source and full canonical hash for each host.
Disable runtime lookup during local search with an empty PATH.
Use 16 balanced pairs for origin to selected. Use eight pairs for independent trials.
Keep snapshot preparation outside query timing. Save all outputs and source hashes.
Do not run beside builds, archive work, or other benchmarks on the same host.

## Limited ASCII typo-distance follow-up

- [x] M14. Compare the OSA recurrence within the existing edit-distance limit.
- [x] V14. Check short-string reference pairs, length boundaries, transpositions, Unicode, and exact outputs.
- [x] D14. Select only after complete synthetic and real-catalog results on both hosts.
- [x] R14. Save all source, samples, CPU, memory, failures, and decisions.

Use three fixed rows. Set cells outside the allowed distance band above the limit.
Keep non-ASCII matching on strsim. Keep the existing right-input length bound.
Use the selected ID-buffer client as the independent baseline.
Run 16 normal pairs, eight million-row pairs, and 260 exact output checks.
Then run 16 pairs per case on the same complete real catalog.
Do not change accepted sources while the earlier queued builds still require them.

## Complete Cask, Formula, and combined search follow-up

- [x] M15. Compare full cached Cask, Formula, and combined queries on both hosts.
- [x] V15. Check exact outputs, complete native rows, immutable data, and empty profiles.
- [x] M16. Profile the accepted Formula, Cask, and combined commands on Linux.
- [x] R15. Keep all samples, instruction profiles, source identities, output, and limits.

Reuse the complete private prepared catalog. Keep its database bytes unchanged.
Use 16 alternating pairs for origin to selected across 15 source and output cases.
Include aliases, unavailable records, library filters, Unicode, and dense output.
Run instruction profiles after timing ends. Compare ordinary and profiled bytes.
Instruction shares do not establish wall-time speed gains.

## Combined fuzzy matching follow-up

- [x] M17. Compare fixed-row distance and duplicate removal as one client.
- [x] V17. Check reference results, synthetic outputs, full real-catalog outputs, and resources.
- [x] D17. Compare the combined client directly with each independent trial before selection.
- [x] R17. Keep all samples, source, failures, and any rejection.

Use the selected ID-buffer source as the starting point.
Run the normal and million-row cases and 260 exact output checks.
Run 16 real-catalog pairs against the selected, fixed-row, and duplicate-only clients.
Do not add the independent percentage changes.

## Typed catalog system decoding follow-up

- [x] M18. Compare typed entry decoding after the full Cask and Formula allocation profile.
- [x] V18. Keep schema, provenance, target, status, version-bound, array-record, and map-section rules.
- [x] D18. Select only after full Rust checks, native lifecycle checks, and both host timings.
- [x] R18. Keep source, every sample, memory, instruction evidence, and any rejection.

The full-catalog profile places substantial instructions in temporary JSON maps and allocation.
Decode the known entries map directly. Ignore unknown system metadata without storing it.
Keep the outer schema decoder and all catalog validation. Keep public TryFrom available.
A map-only visitor must still refuse an array as a system section.
Existing record-array and duplicate-envelope semantics remain covered by the current tests.
Add a behavior check for unknown system metadata.
Run 15 full-source query cases and 13 unchanged native controls with 16 balanced pairs.
Use the actual warm setup command before query timing to refresh source-age reports.
Keep the native database bytes unchanged. Check all pkg cache file hashes around each query matrix.
Build only after the preceding search wave ends. Restore all selected sources before timing.

## Unique exposed-name scan follow-up

- [x] M19. Compare a packages-only scan for newly published catalogs with unique exposed names.
- [x] V19. Keep old metadata, collisions, Unicode, invalid UTF-8, provenance, and integrity behavior.
- [x] D19. Select only after full Rust/native checks and repeated real and synthetic results on both hosts.
- [x] R19. Keep source, all samples, resource passes, fixture changes, and any rejection.

Record an optional unique_exposed field in the publication transaction.
Old metadata defaults to false and keeps the original claims join.
The schema and claims table stay compatible with old clients.
Do not claim a gain for an old snapshot before a new snapshot is prepared.
Real searches use a private clone with verified unique claims and no orphan packages.
Both clients read the same clone. Keep the original complete snapshot unchanged.
Run 16 normal pairs, eight million-row pairs, 16 real pairs, and eight legacy control pairs.
Check 780 exact JSON/text cases across legacy, unique, and ambiguous fixtures per host.
Keep all source and resource guards. Restore accepted sources before native tests and timing.

## Larger bounded-memory search follow-up

- [x] M20. Compare larger native catalogs with streaming fixture and output handling.
- [x] V20. Check exact output hashes, totals, immutable input, separate child resources, and temporary cleanup.
- [x] R20. Keep all samples, fixture shape, disk limits, and failed attempts.

Use tools/search-scale-check.py. First run a small functional check on both hosts.
Generate rows in bounded batches. Do not capture full JSON in Python memory.
Keep output hashing and resource passes outside timed pairs.
Include limited dense queries and a full-output case when the free-space guard permits it.
Distinguish regular-file output and page-cache limits from the ordinary search timings.

## Combined production validation

- [x] M21. Combine the selected search and workflow trials in one default-profile client.
- [x] V21. Run full Rust checks and native lifecycle checks on both hosts with matching source hashes.
- [x] V22. Compare the frozen combined client directly with origin across command, installer, and real catalog cases.
- [x] D21. Keep each new component only after its behavior and direct timing evidence pass.
- [x] R21. Refresh the evidence manifest, preserve failed attempts, and stop all final sandboxes.

Keep individual trial measurements separate. Do not add their percentage changes.
Use one frozen combined binary per host after checks finish.
Include complete native row hashes, exact output, empty final profiles, and separate resources.
Repeat the full shipping installer with normal notices and pinned catalogs.
Keep all slower cases and the limits of local release transport and warm shared stores.
Do not change the Mac source while earlier queued builds still require the selected baseline.

## 2026-10-19 status

The selected components now pass full checks and direct final comparisons on both hosts.
Larger Linux 5M/10M raw files were lost when the disposable host expired; M20–R20 and V22/R21 remain open.
Do not count a missing host result as a pass.
