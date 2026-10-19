# Design

Evaluate a larger count only when the real metadata prototype shows a gain.
Keep one immutable source identity for the complete scan.
Use the existing recursive split after failed or resource-limited evaluation.
Preserve strict path-count and identity checks before writing each result.
A larger batch must stay within the native argument limit.
Compare canonical sorted package rows after full source preparation.
Do not change local search, profile operations, or public result formats.

## Request bounds

Select at most 512 pending paths before grouping them by native output root.
Measure the encoded Nix string literal before starting a child.
Split path lists above 64 KiB in half and preserve their original order.
Reject an oversized single path with a clear decode error.
Keep the existing result-count and exact-path checks after evaluation.

## Garbage collector trials

Keep GC divisor 20 for the selected larger batch.
The direct experiments with divisors 40 and 100 are slower.
Their Darwin-target memory peaks still exceed the production guard.
The direct experiments select a candidate only.
The full product comparison includes the unchanged memory guard and retries.
Require all six complete native row hashes to match on each host.

## Guarded larger queue follow-up

The direct 1,024-path experiment has a small Linux gain and a larger
Darwin-target gain. It does not apply the production memory guard.
Measure a temporary product build with a 1,024-path pending queue.
Keep the existing memory, argument, timeout, output, and retry bounds.
A larger queue can reduce child starts. Resource retries can remove that gain.
The encoded argument limit can also reduce the actual request size.
Compare complete canonical rows and process max RSS before selection.
Measure no-change updates after setup in the same private prepared catalog.
Keep the 512-path production queue unless the full test supports a change.
