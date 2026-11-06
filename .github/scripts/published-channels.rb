require 'base64'
require 'uri'
require_relative 'release-metadata'

# 所有发行线只推进 main 中已公开可下载的候选，保留备版字段
module PublishedChannels
  PATH = 'release/version.yaml'
  LIMIT = 8192

  def self.advance(document, version)
    version = ReleaseMetadata.version(version)
    major, = version.split('.', 2)
    core, stage = version.split('+', 2).first.split('-', 2)
    channels = (document.fetch('published')[major] ||= {})
    if stage&.start_with?('exp.')
      channels = (channels['exp'] ||= {})
      keys = [core]
    else
      stages = %w[stable rc beta alpha]
      keys = stages.drop(stages.index(stage ? stage.split('.').first : 'stable'))
    end
    incoming = ReleaseMetadata.sort_key(version)
    changed = false
    keys.each do |key|
      current = channels[key]
      if current
        comparison = incoming <=> ReleaseMetadata.sort_key(current)
        raise "Conflicting release identities: #{current}, #{version}" if comparison.zero? && current != version
        next unless comparison.positive?
      end
      channels[key] = version
      changed = true
    end
    changed
  end

  def self.api(endpoint, payload = nil)
    args = ['gh', 'api', endpoint]
    args.concat(%w[--method PUT --input -]) if payload
    out, error, status = Open3.capture3(*args, stdin_data: payload ? JSON.generate(payload) : '')
    return :conflict if payload && !status.success? && error.include?('(HTTP 409)')
    raise "Cannot access #{endpoint}: #{error.strip}" unless status.success?
    JSON.parse(out)
  end

  def self.verify_assets(base, version, request)
    tag = "v#{version}"
    release = request.call("#{base}/releases/tags/#{tag}")
    raise "Release is not public or tag identity differs: #{tag}" unless release.fetch('tag_name') == tag &&
      !release.fetch('draft') && release.fetch('prerelease') == ReleaseMetadata.prerelease?(version)
    platforms = request.call("#{base}/contents/release/platforms.yaml?ref=#{URI.encode_www_form_component(tag)}")
    expected = YAML.safe_load(Base64.decode64(platforms.fetch('content'))).fetch('platforms').map do |p|
      ReleaseMetadata.archive_name(version, p)
    end
    assets = release.fetch('assets').select { |a| a.fetch('state') == 'uploaded' && a.fetch('size').positive? }.map { |a| a.fetch('name') }
    missing = (expected + %w[checksums.txt compose.yaml config.example.yaml plugin-release-manifest.json]) - assets
    raise "Incomplete release #{tag}: #{missing.join(', ')}" if expected.empty? || !missing.empty?
  end

  def self.publish(repository, version, &request)
    request ||= method(:api)
    version = ReleaseMetadata.version(version)
    base = "repos/#{repository}"
    verify_assets(base, version, request)
    endpoint = "#{base}/contents/#{PATH}"
    3.times do
      file = request.call("#{endpoint}?ref=main")
      document = YAML.safe_load(Base64.decode64(file.fetch('content')))
      return unless advance(document, version)
      content = YAML.dump(document)
      raise 'Published index exceeds the client size limit' if content.bytesize > LIMIT
      written = request.call(endpoint, { branch: 'main', sha: file.fetch('sha'), content: Base64.strict_encode64(content),
        message: "chore(release): publish channel index for v#{version}" })
      return unless written == :conflict
    end
    raise 'main changed during channel publication; rerun the index step'
  end
end

if $PROGRAM_NAME == __FILE__
  PublishedChannels.publish(ENV.fetch('GITHUB_REPOSITORY'), ARGV.fetch(0))
end
