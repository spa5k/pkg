#!/usr/bin/env ruby
# typed: false
# frozen_string_literal: true

# pkg raw-tap metadata exporter. The export derivation executes the
# pinned Homebrew `bin/brew` (through the controlled boot adapter) so
# upstream shell startup and the committed vendor bundle (no `gem
# install`, no network) provide the runtime:
#
#     "$stage/brew/bin/brew" ruby <this file>
#
# The shell canary/loopback probe has already refused to build unless
# the sandbox denied host read, host write, and loopback connect.
# Full background: IMPLEMENTATION.md.
#
# Contract with the builder (environment, all required):
#   PKG_EXPORT_TAP_PATH     staged tap tree (.../Library/Taps/<owner>/<repo>)
#   PKG_EXPORT_SOURCE       canonical source id `<owner>/<repo>`
#   PKG_EXPORT_REVISION     exact 40-hex tap revision being exported
#   PKG_EXPORT_HOMEBREW_PIN exact 40-hex Homebrew revision (informational)
#   PKG_EXPORT_SYSTEM       the single native target:
#                           aarch64-darwin or x86_64-linux
#   PKG_EXPORT_OUT          output file path (envelope JSON)
#   HOMEBREW_LIBRARY        staging `.../Library` of the writable pinned
#                           Homebrew tree
#
# Security invariants (details: IMPLEMENTATION.md):
#   * Native target only: one fixed simulation per run
#     (target_specs); never to_hash_with_variations, refresh, or both
#     targets in one run.
#   * Each cask file is evaluated in a fresh forked child in its own
#     process group, so global Homebrew state cannot leak between
#     casks and a misbehaving child (or a grandchild it forks) can be
#     SIGKILLed as a whole group.
#   * Producer bounds, enforced by the parent reader BEFORE any JSON
#     parse, come from lib/tap/reader.rb as fixed frozen constants
#     (2 MiB record / 32 MiB total / 60 s per child); there is no
#     environment override (behavioral tests pass short explicit
#     arguments: tests/reader_test.rb).
#   * Each source-evaluating child redirects STDOUT and STDERR to
#     File::NULL before loading the cask: incidental cask prints
#     cannot fill the build/daemon logs. Structured bounded pkg_error
#     records retain failure info.
#   * Cask::DSL#staged_path is guarded during cask-source evaluation
#     (sticky flag + raise); post-load to_h reads get a frozen,
#     never-existing sentinel path that cannot leak the staging root.
#   * Every file yields an explicit ok or error record, each carrying
#     the parent-computed pkg_origin (path + sha256); a run where
#     every file fails is a total failure (non-zero exit, no output).
#
# Namespaces at this pin (cc9ff03…): Tap and Utils::Bottles::Tag (and
# the Cask module: Cask::Cask, Cask::CaskLoader, Cask::DSL) are ROOT
# constants; SimulateSystem lives inside Homebrew
# (Homebrew::SimulateSystem), as does Homebrew::API.

require "pathname"
require "json"
require "digest"

require_relative "reader"
include PkgTapReader

def die(message)
  warn "pkg-export: #{message}"
  exit! 1
end

def one_line(message, limit = 500)
  text = message.to_s.encode(Encoding::UTF_8, invalid: :replace, undef: :replace).scrub.gsub(/\s+/, " ").strip
  text.length > limit ? "#{text[0, limit].scrub('…')}…" : text
end

# Error-record names (cask stems) and diagnostic messages can carry
# non-ASCII bytes. Lossy replacement is allowed ONLY for these
# diagnostic strings, never for the metadata JSON record.
def pkg_utf8(text)
  text.to_s.encode(Encoding::UTF_8, invalid: :replace, undef: :replace).scrub
end

tap_path = Pathname(ENV.fetch("PKG_EXPORT_TAP_PATH") { die "PKG_EXPORT_TAP_PATH is not set" })
source_id = ENV.fetch("PKG_EXPORT_SOURCE") { die "PKG_EXPORT_SOURCE is not set" }
revision = ENV.fetch("PKG_EXPORT_REVISION") { die "PKG_EXPORT_REVISION is not set" }
homebrew_pin = ENV.fetch("PKG_EXPORT_HOMEBREW_PIN") { die "PKG_EXPORT_HOMEBREW_PIN is not set" }
export_system = ENV.fetch("PKG_EXPORT_SYSTEM") { die "PKG_EXPORT_SYSTEM is not set" }
out_path = Pathname(ENV.fetch("PKG_EXPORT_OUT") { die "PKG_EXPORT_OUT is not set" })
library = Pathname(ENV.fetch("HOMEBREW_LIBRARY") { die "HOMEBREW_LIBRARY is not set" })

die "PKG_EXPORT_REVISION must be 40 hex chars (got #{revision.inspect})" unless revision.match?(/\A[0-9a-f]{40}\z/)

homebrew_root = library.parent.realpath.to_s
stage_root = File.dirname(homebrew_root)

# --- boot the pinned Homebrew runtime ------------------------------------
# When executed via `brew ruby`, Homebrew is already loaded and
# everything below is a no-op. A direct `ruby <this file>` boots the
# same pinned tree through upstream standalone.rb (which sets up the
# committed vendor bundle).
begin
  unless defined?(Homebrew) && defined?(Tap) && defined?(Cask) && defined?(Utils)
    require File.join(library, "Homebrew", "standalone.rb")
  end
  require "tap"
  require "api"
  require "simulate_system"
  require "utils/bottles"
  require "cask/cask_loader"
rescue LoadError, StandardError, ScriptError => e
  die "pinned Homebrew runtime did not load (#{one_line(e.message)}): execute via the pinned bin/brew ruby"
end

# Upstream API drift is a total failure, never a silent shape change.
die "pinned API drift: root Tap class missing" unless defined?(Tap) && Tap.is_a?(Class)
die "pinned API drift: Tap#git_head missing" unless Tap.method_defined?(:git_head)
die "pinned API drift: Homebrew::API.with_no_api_env missing" unless defined?(Homebrew::API) &&
  Homebrew::API.respond_to?(:with_no_api_env)
die "pinned API drift: Cask::CaskLoader.load missing" unless defined?(Cask::CaskLoader) &&
  Cask::CaskLoader.respond_to?(:load)
die "pinned API drift: Cask::Cask.generating_hash! missing" unless defined?(Cask::Cask) &&
  Cask::Cask.respond_to?(:generating_hash!)
die "pinned API drift: Cask::Cask#platform_supported? missing" unless defined?(Cask::Cask) &&
  Cask::Cask.method_defined?(:platform_supported?)
die "pinned API drift: Cask::Cask#to_h missing" unless Cask::Cask.method_defined?(:to_h)
die "pinned API drift: Cask::DSL#staged_path missing" unless defined?(Cask::DSL) &&
  Cask::DSL.method_defined?(:staged_path)
die "pinned API drift: Homebrew::SimulateSystem.with missing" unless Homebrew.const_defined?(:SimulateSystem) &&
  Homebrew::SimulateSystem.respond_to?(:with)
die "pinned API drift: Utils::Bottles::Tag.from_symbol missing" unless defined?(Utils::Bottles::Tag) &&
  Utils::Bottles::Tag.respond_to?(:from_symbol)

# The staged tap tree carries no `.git` and the sandbox has no network
# and no `gh`: make upstream Tap git metadata answer inert values. The
# exact-method check above guards this adapter; the known revision is
# the verified revision for this export, and only this staged tree is
# used inside the isolated exporter.
tap_adapter = Module.new do
  define_method(:git_head) { |_options = nil| revision }
  define_method(:git_last_commit) { |_options = nil| nil }
  define_method(:private?) { false }
end
Tap.prepend(tap_adapter)

# --- native target selection (single target, fixed simulation) ------------
target_specs = {
  "aarch64-darwin" => { os: :sequoia, arch: :arm,   tag: :arm64_sequoia },
  "x86_64-linux"   => { os: :linux,   arch: :intel, tag: :x86_64_linux },
}.freeze
spec = target_specs[export_system]
die "PKG_EXPORT_SYSTEM must be aarch64-darwin or x86_64-linux (got #{export_system.inspect})" if spec.nil?

tag = Utils::Bottles::Tag.from_symbol(spec[:tag])
die "Bottles::Tag drift: from_symbol(#{spec[:tag].inspect}) gave #{tag.to_sym.inspect}" unless tag.to_sym == spec[:tag]

tap = Tap.from_path(tap_path)
die "Tap.from_path(#{tap_path}) did not resolve a tap" if tap.nil?
die "staged tap name #{tap.name.inspect} != source #{source_id.inspect}" unless tap.name == source_id

cask_files = Dir.glob(File.join(tap_path, "Casks", "**", "*.rb")).sort
die "no cask files under #{File.join(tap_path, 'Casks')}" if cask_files.empty?

# --- per-cask evaluation in a fresh child process -------------------------
# Returns an explicit record hash: an ok record (upstream to_h plus
# adapter evidence) or an error record with "token" and "pkg_error".
# All producer bounds are enforced here, in the parent, before any
# JSON.parse of child data.
def export_one(file:, tap_path:, spec:, tag:, stage_root:)
  stem = pkg_utf8(File.basename(file, ".rb"))
  rel = Pathname(file).relative_path_from(Pathname(File.join(tap_path, "Casks"))).to_s
  # Parent-computed origin: derived from the exact source file before
  # the fork, attached/overridden on every record after the child
  # returns (ok, load error, deadline kill, invalid UTF-8, unparseable
  # JSON, early exit). A child-supplied pkg_origin is never trusted —
  # the parent value always wins.
  origin = {
    "path"   => File.join("Casks", rel),
    "sha256" => Digest::SHA256.file(file).to_s,
  }
  attach_origin = ->(record) { record["pkg_origin"] = origin; record }
  readable, writable = IO.pipe
  # Explicit binary pipe protocol: read buffers stay ASCII-8BIT
  # regardless of the parent locale.
  readable.binmode
  writable.binmode
  pid = Process.fork do
    Process.setpgrp # own group: SIGKILL covers grandchildren too
    readable.close
    # Log containment: redirect incidental STDOUT+STDERR to
    # File::NULL BEFORE any cask source is loaded, so cask prints
    # cannot fill the build/daemon logs. The metadata pipe (writable)
    # is untouched; bounded pkg_error records carry failures.
    $stdout.reopen(File::NULL)
    $stderr.reopen(File::NULL)
    payload = begin
      # 1. Source-evaluation window: any staged_path touch is fatal
      #    (sticky flag first, so a cask that rescues the raise is
      #    still refused).
      $pkg_staged_path_source_touched = false
      staged_path_guard = Module.new do
        define_method(:staged_path) do |*|
          $pkg_staged_path_source_touched = true
          raise "pkg-export: staged_path called during cask source evaluation"
        end
      end
      Cask::DSL.prepend(staged_path_guard)

      cask = nil
      hash, supported = Homebrew::API.with_no_api_env do
        Cask::Cask.generating_hash!
        Homebrew::SimulateSystem.with(os: spec[:os], arch: spec[:arch]) do
          # 1. Source-evaluation window: any staged_path touch is fatal.
          cask = Cask::CaskLoader.load(Pathname(file))

          # 2. Post-load window: upstream to_h itself reads staged_path
          #    (bundle_version computes an Info.plist path for relative
          #    artifacts). Serve a frozen, never-existing sentinel inside
          #    the build dir instead of the real staging root: globs on it
          #    return nothing, and it cannot leak the staging root.
          # No un-prepend is needed: this fresh child exits right after
          # writing its single record.
          sentinel_guard = Module.new do
            define_method(:staged_path) do |*|
              Pathname("#{stage_root}/pkg-staged-path-sentinel").freeze
            end
          end
          Cask::DSL.prepend(sentinel_guard)
          [cask.to_h, cask.platform_supported?(tag, installable: true)]
        end
      end

      raise "staged-path-during-source-eval: cask source touched Cask::DSL#staged_path" if $pkg_staged_path_source_touched

      record = hash
      record["upstreamPlatformSupported"] = supported ? true : false
      # pkg_origin below is recomputed here only for the staging-root
      # leak check; the parent unconditionally overrides it with its
      # own value after the child returns.
      record["pkg_origin"] = {
        "path" => File.join("Casks", rel),
        "sha256" => Digest::SHA256.file(file).to_s,
      }
      JSON.generate(record).tap do |encoded|
        raise "record embeds the staging root #{stage_root.inspect}" if encoded.include?(stage_root)
      end
    rescue StandardError, ScriptError => e
      JSON.generate({ "token" => stem, "pkg_error" => "#{e.class}: #{one_line(e.message)}" })
    end
    writable.puts payload
    writable.close
    exit! 0 # no at_exit hooks from the Homebrew runtime
  end
  writable.close
  deadline_at = Process.clock_gettime(Process::CLOCK_MONOTONIC) + CHILD_DEADLINE_SECONDS
  line, reason = read_child_line(readable, pid, cap: RECORD_CAP_BYTES, deadline: CHILD_DEADLINE_SECONDS)
  readable.close
  case reason
  when :overflow
    # The reader already SIGKILLed the group; a hung oversized writer
    # cannot block this reap.
    reap_child(pid)
    raise ExportCapExceeded,
          "record for #{stem} exceeded #{RECORD_CAP_BYTES} bytes (per-record cap); " \
          "whole export fails: integrity/resource violation"
  when :deadline
    reap_child(pid)
    return attach_origin.call({ "token" => stem, "pkg_error" => "child exceeded #{CHILD_DEADLINE_SECONDS}s; process group killed" })
  end

  status = wait_child_bounded(pid, deadline_at)
  # Even after a clean child exit, SIGKILL anything left in its group
  # (grandchildren) without an unbounded wait.
  finalize_child_group(pid)
  unless status.is_a?(Process::Status) && status.exited?
    return attach_origin.call({ "token" => stem, "pkg_error" => "child did not exit cleanly after its record (#{status.inspect}); group killed" })
  end

  # Decode BEFORE any JSON parse: invalid UTF-8 becomes a bounded
  # error record here, in the same per-file error path as unparseable
  # JSON. Metadata is never silently scrubbed.
  line, invalid = decode_record_line(line)
  if invalid
    return attach_origin.call({ "token" => stem, "pkg_error" => "child record is not valid UTF-8: #{invalid}" })
  end

  return attach_origin.call({ "token" => stem, "pkg_error" => "export child exited without a record (#{status})" }) if line.empty?

  begin
    JSON.parse(line)
  rescue JSON::ParserError => e
    attach_origin.call({ "token" => stem, "pkg_error" => "child record unparseable: #{one_line(e.message)}" })
  else
    # Parent override: replace any child-supplied pkg_origin with the
    # parent-computed value (the importer verifies it against the
    # captured source inventory).
    attach_origin.call(JSON.parse(line))
  end
end

records = []
files_ok = 0
files_failed = 0
record_bytes_total = 0

begin
  cask_files.each do |file|
    record = export_one(file: file, tap_path: tap_path, spec: spec, tag: tag, stage_root: stage_root)
    # Charge the record against the cumulative cap before the envelope is
    # built (bytes as serialized by the child, newline included).
    record_bytes_total = charge_record_bytes!(record_bytes_total, record.to_json.bytesize + 1,
                                              EXPORT_CAP_BYTES, File.basename(file))
    if record["pkg_error"]
      files_failed += 1
      warn "pkg-export: #{File.basename(file)}: #{record['pkg_error']}"
    else
      files_ok += 1
    end
    records << record
  end
rescue ExportCapExceeded => e
  die e.message
end

die "every cask file failed to export (#{files_failed}/#{cask_files.length})" if files_ok.zero?

envelope = {
  "schema"   => "pkg-tap-raw-export/1",
  "source"   => source_id,
  "revision" => revision,
  "homebrew" => homebrew_pin,
  "ruby"     => RUBY_VERSION,
  "probe"    => ENV.fetch("PKG_EXPORT_PROBE", "unknown"),
  # Single native target for this whole run.
  "system"   => export_system,
  "files"    => { "total" => cask_files.length, "ok" => files_ok, "failed" => files_failed },
  "casks"    => records,
}

# Final byte cap: the serialized envelope must stay within the 32 MiB
# budget (the same cap the Rust consumer enforces).
encoded_envelope = JSON.generate(envelope)
die "serialized envelope is #{encoded_envelope.bytesize} bytes (limit #{EXPORT_CAP_BYTES})" if encoded_envelope.bytesize > EXPORT_CAP_BYTES

tmp = Pathname("#{out_path}.tmp")
tmp.write(encoded_envelope)
File.rename(tmp, out_path)

warn "pkg-export: #{source_id}@#{revision[0, 12]} on #{export_system}: #{files_ok} exported, #{files_failed} failed"
