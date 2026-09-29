# typed: false
# frozen_string_literal: true

# Behavioral tests for the bounded child reader (lib/tap/reader.rb).
# Requires the reader helpers DIRECTLY under plain Ruby: no Homebrew,
# no tap Ruby, no cask evaluation, no test framework. Tests pass SHORT
# caps/deadlines as explicit helper arguments; production always uses
# the fixed frozen constants in reader.rb (asserted below). Run:
#
#   LC_ALL=C ruby nix/casks/lib/tap/tests/reader_test.rb
#
# Checks (9 behavioral):
#   1. normal-record
#   2. error-record + valid control
#   3. huge-pipe-output (overflow; process group killed)
#   4. hang + TERM trap (deadline kills the whole group)
#   5. malformed-json-record
#   6. cumulative-cap
#   7. invalid-utf8-record (real binary bytes)
#   8. non-ascii-valid-record (exact UTF-8 round-trip: 工具 Déjà)
#   9. noisy-child log containment (stdout/stderr discarded to
#      File::NULL, metadata pipe unaffected)

require_relative "../reader"
require "json"
require "tempfile"

FAILURES = []
SHORT_CAP = 4096
SHORT_DEADLINE = 2
SHORT_TOTAL = 65_536

def verify(name, ok, detail = nil)
  if ok
    warn "reader-test: PASS #{name}"
  else
    FAILURES << name
    warn "reader-test: FAIL #{name}#{detail ? " (#{detail})" : ''}"
  end
end

# Production constants are FIXED frozen values; no knob exists.
def constants_fixed?
  PkgTapReader::RECORD_CAP_BYTES == 2_097_152 &&
    PkgTapReader::CHILD_DEADLINE_SECONDS == 60 &&
    PkgTapReader::EXPORT_CAP_BYTES == 33_554_432 &&
    [PkgTapReader::RECORD_CAP_BYTES, PkgTapReader::CHILD_DEADLINE_SECONDS,
     PkgTapReader::EXPORT_CAP_BYTES].all?(&:frozen?)
end

# Test-only child helpers (kept out of production reader.rb):
# semantics identical to what the exporter's own fork does.

# Forks a record child: binmode pipe, own process group. The block runs
# in the child and writes to the given writable; the child exits via
# exit! (no at_exit hooks). Returns [readable, pid] in the parent.
def fork_record_child(&block)
  readable, writable = IO.pipe
  # Binary pipe protocol: buffers stay ASCII-8BIT under LC_ALL=C.
  readable.binmode
  writable.binmode
  pid = Process.fork do
    Process.setpgrp
    readable.close
    block.call(writable)
    writable.close
    exit! 0
  end
  writable.close
  [readable, pid]
end

# True while any process remains in the child's group.
def group_alive?(pid)
  Process.kill(0, -pid)
  true
rescue Errno::ESRCH, Errno::EPERM
  false
end

def read_one(payload_writer, cap: SHORT_CAP, deadline: SHORT_DEADLINE)
  readable, pid = fork_record_child(&payload_writer)
  line, reason = PkgTapReader.read_child_line(readable, pid, cap: cap, deadline: deadline)
  readable.close
  deadline_at = Process.clock_gettime(Process::CLOCK_MONOTONIC) + deadline
  status = PkgTapReader.wait_child_bounded(pid, deadline_at)
  PkgTapReader.finalize_child_group(pid)
  [line, reason, status]
end

def wait_group_dead(pid, tries = 40)
  tries.times do
    return true unless group_alive?(pid)

    sleep 0.05
  end
  !group_alive?(pid)
end

# --- 1. normal record ------------------------------------------------------
begin
  payload = JSON.generate({ "token" => "normal", "ok" => true })
  line, reason, status = read_one(->(w) { w.puts payload })
  rec = reason == :ok ? PkgTapReader.decode_record_line(line).then { |t, _| t && JSON.parse(t) } : nil
  ok = reason == :ok && status.is_a?(Process::Status) && status.exited? && status.exitstatus.zero? &&
       rec == JSON.parse(payload) && constants_fixed?
  verify("normal-record", ok, "reason=#{reason} status=#{status.inspect}")
rescue StandardError => e
  verify("normal-record", false, "#{e.class}: #{e.message}")
end

# --- 2. error record + valid control ---------------------------------------
begin
  err_payload = JSON.generate({ "token" => "bad", "pkg_error" => "RuntimeError: boom" })
  ok_payload = JSON.generate({ "token" => "good", "ok" => true })
  l1, reason1, = read_one(->(w) { w.puts err_payload })
  l2, reason2, = read_one(->(w) { w.puts ok_payload })
  rec1 = reason1 == :ok ? PkgTapReader.decode_record_line(l1).then { |t, _| t && JSON.parse(t) } : nil
  rec2 = reason2 == :ok ? PkgTapReader.decode_record_line(l2).then { |t, _| t && JSON.parse(t) } : nil
  ok = !rec1.nil? && !rec2.nil? && rec1["pkg_error"] && rec2["ok"] == true
  verify("error-record+valid-control", ok, "reason1=#{reason1} reason2=#{reason2}")
rescue StandardError => e
  verify("error-record+valid-control", false, "#{e.class}: #{e.message}")
end

# --- 3. huge pipe output: overflow, process group killed -------------
begin
  readable, pid = fork_record_child do |w|
    w.write("A" * (SHORT_CAP + 1))
    sleep 60 # oversized writer that then hangs: must not block cleanup
  end
  line, reason = PkgTapReader.read_child_line(readable, pid, cap: SHORT_CAP, deadline: SHORT_DEADLINE)
  readable.close
  PkgTapReader.reap_child(pid) # reader killed the group on overflow
  group_dead = wait_group_dead(pid)
  ok = reason == :overflow && line.nil? && group_dead
  verify("huge-pipe-output", ok, "reason=#{reason} group_dead=#{group_dead}")
rescue StandardError => e
  verify("huge-pipe-output", false, "#{e.class}: #{e.message}")
end

# --- 4. hang + TERM trap: deadline SIGKILLs the whole group ----------------
begin
  readable, pid = fork_record_child do |_w|
    Signal.trap("TERM") { } # swallow TERM: only SIGKILL works
    Process.fork { sleep 300 } # hung grandchild in the same group
    sleep 300
  end
  line, reason = PkgTapReader.read_child_line(readable, pid, cap: SHORT_CAP, deadline: 1)
  readable.close
  PkgTapReader.reap_child(pid)
  group_dead = wait_group_dead(pid)
  ok = reason == :deadline && line.nil? && group_dead
  verify("hang+term-trap", ok, "reason=#{reason} group_dead=#{group_dead}")
rescue StandardError => e
  verify("hang+term-trap", false, "#{e.class}: #{e.message}")
end

# --- 5. malformed JSON record: bounded read ok, parse fails ----------------
begin
  line, reason, = read_one(->(w) { w.puts '["not", json' })
  record = begin
    t, = PkgTapReader.decode_record_line(line)
    t && JSON.parse(t)
  rescue JSON::ParserError
    nil
  end
  verify("malformed-json-record", reason == :ok && record.nil?)
rescue StandardError => e
  verify("malformed-json-record", false, "#{e.class}: #{e.message}")
end

# --- 6. cumulative cap ------------------------------------------------------
begin
  raised = false
  begin
    total = PkgTapReader.charge_record_bytes!(0, 100, SHORT_TOTAL, "a")
    PkgTapReader.charge_record_bytes!(total, SHORT_TOTAL, SHORT_TOTAL, "capped")
  rescue PkgTapReader::ExportCapExceeded
    raised = true
  end
  verify("cumulative-cap", raised)
rescue StandardError => e
  verify("cumulative-cap", false, "#{e.class}: #{e.message}")
end

# --- 7. invalid UTF-8 record (real binary bytes) ---------------------------
# Built from real binary bytes (0xE9/0xFF/0xFE): a literal '\xE9'
# string would be plain ASCII and prove nothing.
begin
  raw = String.new(encoding: Encoding::ASCII_8BIT)
  raw << '{"token":"caf' << 0xE9.chr << '-broken","pkg_error":"SyntaxError: bad ' << 0xFF.chr << 0xFE.chr << ' bytes"}'
  raw_invalid = !raw.dup.force_encoding(Encoding::UTF_8).valid_encoding?
  line, reason, = read_one(->(w) { w.write raw })
  text, invalid = reason == :ok ? PkgTapReader.decode_record_line(line) : [nil, nil]
  bounded_error = "child record: #{invalid}"
  ok = raw_invalid && reason == :ok && !invalid.nil? && text.nil? && !bounded_error.empty?
  verify("invalid-utf8-record", ok, "raw_invalid=#{raw_invalid} reason=#{reason} invalid=#{invalid.inspect}")
rescue StandardError => e
  verify("invalid-utf8-record", false, "#{e.class}: #{e.message}")
end

# --- 8. valid non-ASCII record: exact round-trip under LC_ALL=C ------------
begin
  token = "工具 Déjà"
  payload = JSON.generate({ "token" => token, "ok" => true })
  line, reason, = read_one(->(w) { w.puts payload.b })
  text, invalid = reason == :ok ? PkgTapReader.decode_record_line(line) : [nil, "no read"]
  rec = text && JSON.parse(text)
  ok = invalid.nil? && rec && rec["token"] == token && rec["token"].encoding == Encoding::UTF_8
  verify("non-ascii-valid-record", ok, "reason=#{reason} invalid=#{invalid.inspect}")
rescue StandardError => e
  verify("non-ascii-valid-record", false, "#{e.class}: #{e.message}")
end

# --- 9. noisy child: stdout/stderr discarded, metadata still arrives -------
# Same containment technique as the production export child: the child
# redirects STDOUT+STDERR to File::NULL, prints 2 MiB of noise, and
# still writes its metadata record through the pipe. The TEST PROCESS
# redirects its own stdout/stderr to a temp file so we can PROVE that
# none of the noise reaches the parent's log streams.
begin
  log = Tempfile.new("pkg-reader-test-log")
  log.close
  stdout_dup = $stdout.dup
  stderr_dup = $stderr.dup
  $stdout.reopen(log.path, "w")
  $stderr.reopen(log.path, "w")
  begin
    payload = JSON.generate({ "token" => "noisy", "ok" => true })
    line, reason, status = read_one(
      lambda do |w|
        $stdout.reopen(File::NULL)
        $stderr.reopen(File::NULL)
        $stdout.write("noise" * 419_430) # ~2 MiB to stdout
        $stderr.write("noise" * 419_430) # ~2 MiB to stderr
        w.puts payload
      end
    )
  ensure
    $stdout.reopen(stdout_dup)
    $stderr.reopen(stderr_dup)
    stdout_dup.close
    stderr_dup.close
  end
  rec = reason == :ok ? PkgTapReader.decode_record_line(line).then { |t, _| t && JSON.parse(t) } : nil
  log_bytes = File.size(log.path)
  log.unlink
  ok = reason == :ok && status.is_a?(Process::Status) && status.exited? && status.exitstatus.zero? &&
       rec == JSON.parse(payload) && log_bytes.zero?
  verify("noisy-child-containment", ok, "reason=#{reason} log_bytes=#{log_bytes}")
rescue StandardError => e
  verify("noisy-child-containment", false, "#{e.class}: #{e.message}")
end

if FAILURES.empty?
  warn "reader-test: ALL PASS (9 checks)"
  exit! 0
end
warn "reader-test: FAILURES: #{FAILURES.join(', ')}"
exit! 1
