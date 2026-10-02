require 'json'

# 只在正式发版时推进弃用窗口，不把版本号差值当作发布次数。
module PluginApiLifecycle
  def self.advance(entries, host, version)
    next_entries = entries.map(&:dup)
    stable = version.match?(/\A\d+\.\d+\.\d+(?:\+[0-9A-Za-z.-]+)?\z/)
    release = version.split('+').first
    supported = host.fetch('capabilities').to_h { |entry| [entry.fetch('capability'), entry.fetch('versions')] }
    next_entries.reject! do |entry|
      capability = entry.fetch('capability')
      old = entry.fetch('version')
      replacement = entry.fetch('replacement_version')
      remaining = entry.fetch('remaining_releases')
      raise "#{capability}: invalid compatibility window" unless remaining.is_a?(Integer) && remaining >= 0

      versions = supported.fetch(capability, [])
      unless versions.include?(replacement)
        raise "#{capability}: replacement v#{replacement} must be available during migration"
      end
      unless versions.include?(old)
        raise "#{capability} v#{old}: #{remaining} promised stable releases remain" unless remaining.zero?
        next true
      end
      next false unless stable
      next false if entry.fetch('last_counted_in') == release

      if entry.fetch('introduced_in').nil?
        entry['introduced_in'] = release
      else
        if remaining.zero?
          raise "#{capability} v#{old}: compatibility window is complete; remove the old contract and adapter before publishing"
        end
        entry['remaining_releases'] -= 1
      end
      entry['last_counted_in'] = release
      false
    end
    next_entries
  end
end

if $PROGRAM_NAME == __FILE__
  begin
    path, host_path, version = ARGV
    raise 'usage: plugin-api-lifecycle.rb <deprecations.json> <host-compatibility.json> <version>' unless ARGV.length == 3

    entries = JSON.parse(File.read(path))
    updated = PluginApiLifecycle.advance(entries, JSON.parse(File.read(host_path)), version)
    File.write(path, "#{JSON.pretty_generate(updated)}\n") unless updated == entries
  rescue StandardError => error
    warn "error: #{error.message}"
    exit 1
  end
end
