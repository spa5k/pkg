# DN-16 macOS runner and cost preflight

Date: 2026-08-29

Scope: read-only review of the DN-16 proof workflow, local Tart capacity,
GitHub runner state, and current provider prices. No VM, runner, workflow,
release, or cloud resource was created.

## Decision

Do not buy or register anything yet.

The current proof cannot complete even if two runners are online. Two signed
DN-16 release inputs, an immutable staged N/N+1 channel, and a two-phase reboot
proof are still absent. The proof harness records the last two items as
externally blocked and ends with a failed result.

When those inputs are ready, use the existing local M4 Mac mini and run the two
lifecycle slots sequentially. Create, run, and destroy the slot 1 Tart VM.
Then create, run, and destroy a fresh slot 2 Tart VM. This has zero new provider
cost. It meets the repository's current contract, which requires two different
`VirtualMac` guests, runner names, and instance nonces. The contract does not
prove that the guests use two different physical Macs.

The strict sequential storage gate is about 70 GiB: one 50 GB guest plus a
20 GiB host reserve. The host now has about 74 GiB free, so this gate is
satisfied. A strict simultaneous two-guest gate is about 120 GiB. The current
host does not satisfy that gate. Monitor free space during each lifecycle run.

If two different physical Macs are a new requirement, the lowest published
external price found is two Scaleway M1 Mac minis for a 24-hour minimum. The
minimum compute charge is `2 × 24 × €0.11 = €5.28` before tax. Each physical Mac
must run one Tart VM because the workflow refuses bare-metal runner hosts.

## Repository contract

The exact workflow is [`.github/workflows/macos-alpha-proof.yml`](../../../.github/workflows/macos-alpha-proof.yml).
Its external contract is [`tests/macos-clean-host/README.md`](../../../tests/macos-clean-host/README.md).

The destructive matrix requires all of these labels:

- `self-hosted`
- `macOS`
- `ARM64`
- `pkg-disposable-macos-proof-1` or `pkg-disposable-macos-proof-2`

The custom labels must map to these exact runner names:

- `pkg-dn16-proof-runner-1`
- `pkg-dn16-proof-runner-2`

The workflow then verifies these facts before it downloads candidates or
changes the machine:

- the event is a manual dispatch;
- the destructive confirmation has the exact value;
- the production flag is false;
- the runner is self-hosted;
- the runner name is exact;
- the system is arm64 macOS;
- `kern.hv_vmm_present` is `1`;
- `hw.model` starts with `VirtualMac`;
- three root-owned, mode `0600` markers are exact and fresh;
- passwordless `sudo` works;
- Nix, pkg, their users, and their launchd jobs are absent;
- `gh`, `cosign`, and `python3` are available.

The aggregate job proves that the two runner names and the two 64-character
lowercase hexadecimal instance nonces differ. It does not read or compare a
physical-host identity. Therefore one physical M4 host with two fresh,
sequential Tart guests satisfies the current code. It does not satisfy a
stronger two-physical-host claim.

GitHub routes a self-hosted job only to an online, idle runner that matches all
requested labels. The standard runner registration assigns `self-hosted`, an
operating-system label, and an architecture label. Custom labels can be
supplied during configuration. GitHub also states that it accepts labels as
given and does not validate that they match the runner's operating system or
architecture. Labels are routing metadata, not hardware attestation. The
workflow's native checks remain necessary. [GitHub label routing documentation](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/use-in-a-workflow)
[GitHub label assignment documentation](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/apply-labels)

## Current local state

Read-only commands gave this result:

| Item | Observed value |
|---|---:|
| Host | Apple M4 Mac mini |
| Host CPU and memory | 10 cores, 32 GiB |
| Free host storage | About 74 GiB on `/` and `/System/Volumes/Data` |
| Tart | 2.35.0 installed; 2.36.0 current |
| Local Tart guests | 0 |
| Cached image | `ghcr.io/cirruslabs/macos-sequoia-base@sha256:3f4d14a5ffb9efd3bda2ae0184fd4bc2773d924ff8b7565f958761420ec41a0c` |
| Cached image logical disk | 50 GB |
| Cached image reported use | 33 GB |
| Physical Tart cache use | 31 GiB |
| Registered repository runners | 0 |
| Workflow state on GitHub | `disabled_manually` |
| Repository visibility | public |

The installed `tart clone --help` states that local clones use APFS
copy-on-write. A new clone therefore starts with a small unique-storage cost.
Only blocks changed by a clone consume new physical space.

Copy-on-write can make the actual clone cost much smaller than its logical
disk. The strict gate does not depend on that saving. It reserves the full
50 GB logical disk for the active guest and 20 GiB for the host. The sequential
gate is therefore about 70 GiB, and the current 74 GiB satisfies it. Running two
50 GB guests together would require a strict gate of about 120 GiB. The host
does not satisfy that simultaneous gate. The runner, candidates, Determinate
Nix install, logs, APFS metadata, and normal host writes still require runtime
monitoring.

Tart defaults to two CPUs and 4 GB of memory per guest. The destructive guests
do not compile Rust; the GitHub-hosted harness job compiles the proof
executable. Keep one default guest active at a time. Do not increase guest CPU
or memory without evidence.

Tart's current quick start lists the Sequoia base image, `admin`/`admin` guest
credentials, SSH access, and a two-CPU, 4 GB default guest. [Tart quick start](https://tart.run/quick-start/)
The installed Tart 2.35.0 is one release behind 2.36.0. Upgrade Tart and verify
version 2.36.0 before any clone or runner provisioning. [Tart 2.36.0 release](https://github.com/openai/tart/releases/tag/2.36.0)

The local ten-core host is below Tart's published 100-host-core free tier.
[Tart licensing](https://tart.run/licensing/)

## External provider comparison

Prices are direct provider prices as of the date above. Taxes, storage, data
transfer, and setup labor are excluded unless stated.

| Option | Minimum published compute cost for this proof | Two fresh Apple Silicon environments | Meets the current DN-16 workflow without weakening it |
|---|---:|---|---|
| Existing local M4 + sequential Tart VMs | $0 new provider cost | Yes, two fresh VMs run one at a time on one physical Mac | Yes, after the Tart upgrade and proof inputs are ready |
| GitHub standard hosted macOS | $0 for this public repository | Yes, each job gets a fresh VM | No |
| Apple Xcode Cloud | $0 incremental if the existing developer membership has unused included hours | It provides ephemeral build environments | No |
| Scaleway, one M2 physical Mac + two Tart VMs | `24 × €0.17 = €4.08` before tax | Yes, two VMs on one physical Mac | In principle; needs setup and validation |
| Scaleway, two M1 physical Macs + one Tart VM each | `2 × 24 × €0.11 = €5.28` before tax | Yes, two VMs on two physical Macs | In principle; this is the cheapest stronger physical-host option found |
| AWS, one `mac2` M1 Dedicated Host + two Tart VMs | `24 × $0.65 = $15.60`, plus EBS | Not established for Tart on Apple Silicon EC2 Mac | Not established by the cited AWS documentation |
| AWS, two `mac2` M1 Dedicated Hosts + one Tart VM each | `2 × 24 × $0.65 = $31.20`, plus EBS | Not established for Tart on Apple Silicon EC2 Mac | Not established by the cited AWS documentation |
| MacStadium, one M4.S physical Mac + two sequential Tart VMs | $149 per month | Yes, two VMs on one physical Mac | In principle |
| MacStadium, two M2.S physical Macs + one Tart VM each | `2 × $109 = $218` per month | Yes, two VMs on two physical Macs | In principle |

### GitHub-hosted macOS

Standard GitHub-hosted runners are free and unlimited for public repositories.
Each normal job gets a new VM. The arm64 `macos-15` runner has three M1 CPU
cores, 7 GB of memory, and 14 GB of storage. [GitHub hosted-runner reference](https://docs.github.com/en/actions/how-tos/write-workflows/choose-where-workflows-run/choose-the-runner-for-a-job)

It is not valid for this proof. The workflow requires
`RUNNER_ENVIRONMENT=self-hosted`, exact runner names, provisioner-created root
markers, and a controlled reboot before the job. Replacing the workflow's
labels would remove evidence, not only reduce cost.

### Apple Xcode Cloud

Apple Developer Program membership includes 25 Xcode Cloud compute hours per
month. Xcode Cloud destroys its temporary build environments after builds.
[Apple Xcode Cloud pricing](https://developer.apple.com/xcode-cloud/)

It is not valid for this proof. Apple says custom build scripts cannot obtain
administrator privileges with `sudo`. DN-16 needs root-owned markers, native
package installation, launchd inspection, Nix installation, and terminal
uninstall. [Apple dependency and privilege documentation](https://developer.apple.com/documentation/Xcode/Making-Dependencies-Available-to-Xcode-Cloud)

### Scaleway

Scaleway publishes Apple Silicon bare-metal prices of €0.11/hour for an 8 GB
M1 and €0.17/hour for a 16 GB M2. [Scaleway Apple Silicon pricing](https://www.scaleway.com/en/pricing/apple-silicon/)
Its documented minimum macOS lease is 24 hours. [Scaleway Apple Silicon FAQ](https://www.scaleway.com/en/docs/apple-silicon/faq/)

The rented system is bare metal. It must host Tart because DN-16 requires a
`VirtualMac`. For the current two-VM contract, one 16 GB M2 is the smallest
reasonable external configuration. For physical independence, use two M1
hosts and run one 4 GB Tart guest on each.

Availability is a real risk. The product is limited to Scaleway's Paris zone,
and inventory can change. Account verification, tax, image download time, and
runner setup are not included in the price.

### AWS EC2 Mac

AWS EC2 Mac uses bare-metal Dedicated Hosts. AWS requires a 24-hour minimum
allocation, and one Mac instance runs on each Dedicated Host. [AWS EC2 Mac product and billing](https://aws.amazon.com/ec2/instance-types/mac/)

The current AWS public price-list snapshot for US East (N. Virginia) lists the
M1 `mac2` Dedicated Host at $0.65 per hour. The calculations above use that
rate. [AWS public EC2 price list, 2026-08-28](https://pricing.us-east-1.amazonaws.com/offers/v1.0/aws/AmazonEC2/20260828194117/us-east-1/index.json)

EBS root volumes add cost. Capacity quotas and host availability can also
block immediate allocation. AWS scrubs an Apple Silicon host after stop or
termination, and the scrub can take up to 4.5 hours. Billing pauses during the
scrub, but a host cannot be released before the 24-hour minimum. [AWS Mac stop and release behavior](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/mac-instance-stop.html)

AWS Mac is bare metal, so Tart must still create the `VirtualMac` guests. The
cited AWS FAQ documents type-2 virtualization only for x86 EC2 Mac. Its Apple
Silicon statement says that older macOS versions cannot run; it does not state
that Tart or Apple's Virtualization framework is supported on Apple Silicon
EC2 Mac. This review must not present AWS as a supported DN-16 runner option.
It remains unqualified until a separate native preflight proves the exact
combination. [AWS EC2 Mac virtualization FAQ](https://aws.amazon.com/ec2/instance-types/mac/faqs/)

### MacStadium

MacStadium publishes $109/month for an 8 GB M2.S Mac mini and $149/month for a
16 GB M4.S Mac mini. The M4.S is the one-host comparison because it has enough
memory for the host and a sequential 4 GB Tart guest. MacStadium does not offer
a free trial for individual dedicated Macs. [MacStadium pricing](https://macstadium.com/pricing)

Orka supports Apple Silicon VMs and allows up to two VMs per Apple Silicon
node. [MacStadium Orka Apple Silicon support](https://docs.macstadium.com/orka/orka-resources/apple-silicon-based-support)
Orka pricing is not public on the cited page. The table therefore uses bare
metal rental plus Tart, not an unpriced Orka contract.

MacStadium is a poor one-run value. Its monthly term is much larger than the
Scaleway or AWS 24-hour minimum.

## Minimal safe local setup

Do these steps only after the three non-runner proof blockers are resolved and
after explicit authority is given to enable a workflow, register runners, and
dispatch a destructive run.

1. Confirm that host free space is still at least 70 GiB. This is one 50 GB
   guest plus a 20 GiB host reserve. The current reading is about 74 GiB, so
   the sequential gate is satisfied.
2. Upgrade Tart from 2.35.0 to 2.36.0. Verify the installed version before any
   clone or runner provisioning.
3. Clone the cached image by immutable digest for lifecycle slot 1 only.
4. Keep the default two CPUs and 4 GB guest memory. Run the guest headless. Do
   not expose host directories to it.
5. In the guest, verify `VirtualMac`, arm64, passwordless `sudo`, and the exact
   clean-state checks already present in the workflow.
6. Install only `gh`, `cosign`, `python3`, and the official arm64 Actions
   runner if any is absent. Do not install Nix.
7. Download the Actions runner from its official release and verify its
   published SHA-256 before extraction. The current arm64 macOS asset is
   `actions-runner-osx-arm64-2.337.0.tar.gz`, 127,732,571 bytes, with SHA-256
   `5a2cd92908a93d7276a194e1de6008099f3e7946f3f8e14aa7a1a7b4a31fdec2`.
   [Actions runner 2.337.0](https://github.com/actions/runner/releases/tag/v2.337.0)
8. Leave the runner unregistered and stopped while the hosted harness builds.
9. Enable the manually disabled workflow, then dispatch the proof branch with
   the exact confirmation, `production_environment=false`, and the two signed
   release tags.
10. Get the GitHub run ID. Wait until the hosted `harness` job passes and both
    destructive jobs are queued.
11. Create one fresh repository registration token for slot 1. Register the
    guest as `pkg-dn16-proof-runner-1`, with only its custom slot label and
    `--ephemeral`. Do not use `--replace`.
12. Configure an `ACTIONS_RUNNER_HOOK_JOB_STARTED` script outside the Actions
    runner directory. Give it the fixed run ID and lifecycle slot. The hook
    must use `sudo -n` to write the disposable marker as `root:wheel` mode
    `0600`. GitHub runs this hook synchronously after assignment and before the
    job starts. [GitHub pre-job hook documentation](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/run-scripts)
13. Generate a 32-byte random nonce for slot 1. Write the exact instance and
    reboot records as `root:wheel` mode `0600`. Record the current boot UUID in
    the reboot record.
14. Reboot the slot 1 guest immediately.
15. Resolve its IP again. Start `run.sh` only after the reboot. A service is not
    needed for one ephemeral job.
16. Confirm that only `pkg-dn16-proof-runner-1` is online. Let slot 1 assign.
    The pre-job hook writes the third marker before workflow steps start.
17. Monitor free disk space and the runner log. Stop if host free space
    approaches the 20 GiB reserve.
18. After slot 1 evidence uploads, stop and delete its VM. Confirm that its
    ephemeral runner registration disappeared.
19. Confirm that free space is again at least 70 GiB. Clone a new guest from
    the same immutable image for lifecycle slot 2.
20. Repeat steps 4 through 17 with exact runner name
    `pkg-dn16-proof-runner-2`, the slot 2 custom label, a new registration
    token, and a different 32-byte nonce. Never reuse the slot 1 VM or nonce.
21. After slot 2 evidence uploads, stop and delete its VM. Confirm that its
    ephemeral registration disappeared. Remove any stale offline registration
    through GitHub before declaring cleanup complete.

The five-minute marker limit controls steps 13 through the slot 1 preflight and
the equivalent slot 2 steps. Do not create either slot's markers when the
workflow is first dispatched. The hosted harness can run for up to 30 minutes,
so early markers will be stale.

GitHub recommends `--ephemeral` for one-job runners. It automatically
deregisters an ephemeral runner after one job. Registration tokens expire after
one hour. [GitHub self-hosted runner reference](https://docs.github.com/en/actions/reference/runners/self-hosted-runners)
[GitHub runner registration documentation](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/add-runners)

## Credentials and secrets

Required:

- A GitHub identity with repository administrator access.
- A host-side token that can create repository runner registration tokens.
  The current `gh` login has repository and workflow scopes, and the current
  GitHub API reports administrator access.
- Two short-lived runner registration tokens. Create them after the hosted
  harness passes. Do not store them in the repository or in a VM image.
- Guest administrator access. The cached public Tart image documents
  `admin`/`admin`; treat this as disposable bootstrap access only.
- Passwordless `sudo` inside each disposable guest.

Not required on the destructive guests:

- No long-lived GitHub personal access token.
- No repository secret.
- No signing private key.
- No Apple Developer credential.
- No Determinate credential.

GitHub supplies a job-scoped token to the workflow. The workflow grants only
`contents: read`. Cosign performs public keyless identity verification. The
signed releases must already exist before runner setup.

GitHub states that a repository registration token expires after one hour.
For a public repository, a classic token needs `public_repo`; a `repo` token
also covers it. A fine-grained token needs repository Administration write
permission. [GitHub REST runner-token documentation](https://docs.github.com/en/rest/actions/self-hosted-runners#create-a-registration-token-for-a-repository)

## Blocking facts found in this preflight

1. GitHub reports zero registered repository runners.
2. GitHub reports the existing default-branch workflow as
   `disabled_manually`. Enabling it is a separate external mutation.
3. The updated proof file exists on the backed-up proof branch, while the
   default branch still contains the older proof definition. GitHub permits a
   manual run with `--ref`, but the workflow file must exist on the default
   branch. It does exist there. [GitHub manual-run documentation](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow)
4. No current release tag resolves to reviewed DN-16 commit
   `8ffd325a4be12a998f3a5684097b57841a11540e`. Public alpha.7 resolves to
   `2d5cbe178cdf4367f9d3dca216f6c6c13166817c`. Draft alpha.6 resolves to
   `a1895824c9b44c07f277f63b35afaeebc11db6ee`.
5. The workflow requires two different signed tags that both resolve to the
   reviewed DN-16 commit and contain distinct authenticated packages and
   release IDs. Those inputs do not exist.
6. Both packages currently use one live product channel. They cannot prove a
   real N-to-N+1 transition without immutable staged channel states.
7. The current workflow proves a fresh-runner reboot. It does not prove
   product lifecycle recovery across a reboot. That needs a separate
   two-phase runner protocol.
8. The public repository increases self-hosted runner risk. GitHub recommends
   self-hosted runners only for private repositories because forked code can
   be dangerous. The narrow defense here is manual dispatch, exact unique
   labels, ephemeral registration after the trusted harness passes, no
   repository secrets, and immediate VM destruction. [GitHub self-hosted runner security warning](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/add-runners)

## Final recommendation

Use the local sequential lifecycle-slot plan. Upgrade Tart to 2.36.0. Run and
destroy slot 1 before creating slot 2. The current 74 GiB satisfies the 70 GiB
sequential gate, but not the 120 GiB simultaneous gate. This is the smallest
correct solution and has no new provider cost. Monitor space during each run.

Do not weaken the workflow to use free GitHub-hosted or Xcode Cloud machines.
They cannot produce the required root, reboot, and self-hosted evidence.

Do not rent cloud Macs until the signed inputs and staged-channel protocol are
ready. If physical-host independence becomes mandatory, use two Scaleway M1
hosts for the one-day proof. Recheck live inventory and the checkout estimate
immediately before purchase.
