require 'fileutils'
require 'json'
require 'open3'
require 'rubygems'
require 'time'
require 'yaml'

# 本地入口与发行工作流共用版本、说明和平台合同
module ReleaseMetadata
  APP_NAME = 'codex-proxy-rs'
  SEMVER = /\A(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-(?:alpha|beta|rc|exp)\.[1-9]\d*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?\z/

  def self.version(raw)
    value = raw.delete_prefix('v')
    raise "Invalid release version: #{raw}" unless value.match?(SEMVER)
    value
  end

  def self.prerelease?(value)
    version(value).split('+', 2).first.include?('-')
  end

  def self.sort_key(value)
    Gem::Version.new(version(value).split('+', 2).first)
  end

  def self.validate_notes(value)
    tag = "v#{version(value)}"
    lines = File.readlines('release/notes.md')
    unless lines.first&.strip == "# #{tag}" && !lines.drop(1).join.strip.empty?
      raise "release/notes.md: expected non-empty notes starting with '# #{tag}'"
    end
  end

  def self.git(*arguments)
    output, status = Open3.capture2('git', *arguments)
    raise "git #{arguments.join(' ')} failed" unless status.success?
    output.strip
  end

  def self.platforms
    entries = YAML.load_file('release/platforms.yaml').fetch('platforms')
    raise 'release/platforms.yaml: platforms must be a non-empty array' unless entries.is_a?(Array) && !entries.empty?
    seen = {}
    entries.each do |entry|
      raise 'Invalid release platform' unless entry.is_a?(Hash)
      %w[os arch runner target binary archive].each do |key|
        raise "Invalid platform #{key}" unless entry[key].is_a?(String) && !entry[key].strip.empty?
      end
      key = "#{entry['os']}-#{entry['arch']}"
      raise "Duplicate release platform: #{key}" if seen[key]
      seen[key] = true
      raise "Unsupported archive: #{entry['archive']}" unless entry['archive'] == 'tar.gz'
      if entry['docker'] == true
        platform = entry['dockerPlatform']
        raise "Missing Docker platform: #{key}" unless platform.is_a?(String) && !platform.strip.empty?
        entry['dockerPlatform'] = platform.strip
      end
      entry['artifact'] = "#{APP_NAME}-#{key}"
    end
    raise 'At least one Docker platform is required' unless entries.any? { |entry| entry['docker'] == true }
    entries
  end

  def self.outputs(tag)
    value = version(YAML.load_file('release/version.yaml').fetch('version'))
    raise "Release tag #{tag} does not match v#{value}" unless tag == "v#{value}"
    validate_notes(value)
    entries = platforms
    docker, native = entries.partition { |entry| entry['docker'] == true }
    previous = git('tag', '--list', 'v*', '--merged', 'HEAD').lines.map(&:strip).filter_map do |candidate|
      next unless candidate.delete_prefix('v').match?(SEMVER)
      key = sort_key(candidate)
      [key, candidate] if key < sort_key(value)
    end.max_by(&:first)&.last
    {
      'tag' => tag,
      'version' => value,
      'docker_version' => value.tr('+', '-'),
      'build_time' => Time.now.utc.iso8601,
      'git_sha' => git('rev-parse', 'HEAD'),
      'platform_matrix' => JSON.generate('include' => entries),
      'docker_matrix' => JSON.generate('include' => docker),
      'non_docker_matrix' => JSON.generate('include' => native),
      'windows_matrix' => JSON.generate('include' => entries.select { |entry| entry['os'] == 'windows' }),
      'docker_platforms' => docker.map { |entry| entry['dockerPlatform'] }.join(','),
      'previous_tag' => previous,
      'prerelease' => prerelease?(value),
      'build_type' => value.split('+', 2).first.include?('-exp.') ? 'experimental' : 'release'
    }
  end

  def self.seal_manifest(metadata)
    path = 'backend/crates/gateway-plugin/runtime/plugin-host-compatibility.json'
    host = JSON.parse(File.read(path))
    raise "Invalid plugin host compatibility declaration: #{path}" unless host['schema_version'] == 2
    manifest = {
      'schema_version' => 1,
      'sealed' => true,
      'gateway_version' => metadata.fetch('version'),
      'gateway_git_sha' => metadata.fetch('git_sha'),
      'plugin_host' => host,
      'plugins' => []
    }
    FileUtils.mkdir_p('dist/plugin-release-assets')
    File.write('dist/plugin-release-assets/plugin-release-manifest.json', JSON.pretty_generate(manifest) + "\n")
  end
end

if $PROGRAM_NAME == __FILE__
  begin
    command, value = ARGV
    raise 'usage: release-metadata.rb validate VERSION | outputs TAG' unless ARGV.length == 2
    case command
    when 'validate'
      value = ReleaseMetadata.version(value)
      ReleaseMetadata.validate_notes(value)
      puts value
    when 'outputs'
      metadata = ReleaseMetadata.outputs(value)
      ReleaseMetadata.seal_manifest(metadata)
      metadata.each { |key, entry| puts "#{key}=#{entry}" }
    else
      raise "Unknown command: #{command}"
    end
  rescue StandardError => error
    warn "error: #{error.message}"
    exit 1
  end
end
