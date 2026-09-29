# typed: false
# frozen_string_literal: true

# pkg raw-tap bounded child reader (shared by export.rb and
# tests/reader_test.rb). Production bounds are fixed frozen constants
# (no environment override):
#   RECORD_CAP_BYTES        2 MiB  per serialized child record
#   EXPORT_CAP_BYTES       32 MiB  cumulative records + final envelope
#   CHILD_DEADLINE_SECONDS  60 s   per child (read + bounded exit wait)
# The record reader takes cap and deadline as explicit arguments so
# behavioral tests can pass short values; production callers always
# pass the constants. Details: IMPLEMENTATION.md.
module PkgTapReader
  RECORD_CAP_BYTES = 2_097_152.freeze
  CHILD_DEADLINE_SECONDS = 60.freeze
  # Matches the 32 MiB cap the Rust consumer enforces on export.json.
  EXPORT_CAP_BYTES = 33_554_432.freeze

  # Cumulative-cap / overflow violation. export.rb turns this into a
  # whole-export failure; tests rescue it to assert the mapping.
  class ExportCapExceeded < StandardError; end

  module_function

  # The child runs in its own process group (setpgrp in the child), so
  # this kills the child and any grandchildren, even if the child
  # trapped SIGTERM.
  def kill_child_group(pid)
    Process.kill("KILL", -pid) # whole process group
  rescue Errno::ESRCH, Errno::EPERM
    begin
      Process.kill("KILL", pid)
    rescue Errno::ESRCH, Errno::EPERM
      nil
    end
  end

  def reap_child(pid)
    Process.wait2(pid)
  rescue Errno::ECHILD, Errno::EINVAL
    [nil, nil]
  end

  # After a child exits, kill anything still in its group so
  # grandchildren cannot outlive the export. Best-effort: grandchildren
  # are not our children, so the reap cannot block.
  def finalize_child_group(pid)
    kill_child_group(pid)
    reap_child(pid)
  end

  # Reads one record line from the child pipe under a byte cap and a
  # under a byte cap and a monotonic deadline. Returns [line, reason]
  # with reason :ok, :overflow (cap exceeded; group SIGKILLed at once,
  # caller must fail the whole export), or :deadline (group also
  # SIGKILLed; caller records a per-file error). IO.select +
  # read_nonblock; no unbounded gets.
  def read_child_line(readable, pid, cap:, deadline:)
    deadline_at = Process.clock_gettime(Process::CLOCK_MONOTONIC) + deadline
    buf = String.new(capacity: 8192, encoding: Encoding::ASCII_8BIT)
    loop do
      remaining = deadline_at - Process.clock_gettime(Process::CLOCK_MONOTONIC)
      if remaining <= 0
        kill_child_group(pid)
        return [nil, :deadline]
      end

      ready, = IO.select([readable], nil, nil, remaining)
      next unless ready

      loop do
        chunk = readable.read_nonblock(8192, exception: false)
        case chunk
        when :wait_readable
          break
        when nil # EOF: the child side of the pipe is closed
          return [buf, :ok]
        else
          buf << chunk
          if buf.bytesize > cap
            kill_child_group(pid)
            return [nil, :overflow]
          end
        end
      end
    end
  end

  # Reinterprets raw child bytes (ASCII-8BIT) as UTF-8 with no
  # conversion, so valid multi-byte sequences are preserved exactly.
  # Returns [text, nil] for valid UTF-8 or [nil, reason] otherwise;
  # metadata is never silently scrubbed.
  def decode_record_line(line)
    text = line.dup.force_encoding(Encoding::UTF_8)
    return [nil, "invalid UTF-8 byte sequence in child record"] unless text.valid_encoding?

    [text.strip, nil]
  end

  # Bounded wait for the child to exit after EOF. A child that closed
  # its pipe but hangs is SIGKILLed as a group at the deadline and
  # reported as :killed, never allowed to block the export.
  def wait_child_bounded(pid, deadline_at)
    loop do
      _, status = Process.wait2(pid, Process::WNOHANG)
      return status if status

      remaining = deadline_at - Process.clock_gettime(Process::CLOCK_MONOTONIC)
      if remaining <= 0
        kill_child_group(pid)
        _, status = reap_child(pid)
        return status || :killed
      end
      IO.select(nil, nil, nil, [remaining, 0.05].min)
    end
  rescue Errno::ECHILD
    nil
  end

  # Cumulative bound, charged as records accumulate so the envelope is
  # never built past the budget. Raises ExportCapExceeded or returns
  # the new running total.
  def charge_record_bytes!(total, bytes, cap, stem)
    total += bytes
    raise ExportCapExceeded,
          "cumulative record bytes (#{total}) exceeded the #{cap}-byte export cap at #{stem}; whole export fails" if total > cap

    total
  end
end
