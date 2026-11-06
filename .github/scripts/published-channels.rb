require 'base64'
require 'digest'
require 'uri'
require_relative 'release-metadata'

# 通道快照随版本封存，固定 Release 只记录已公开快照和可恢复的待发布摘要
module PublishedChannels
  POINTER = 'update-index'
  LIMIT = 128 * 1024

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

  class Publisher
    def api(endpoint, method: 'GET', data: nil, optional: false)
      args = ['gh', 'api', endpoint, '--method', method]
      args.concat(%w[--input -]) if data
      out, error, status = Open3.capture3(*args, stdin_data: data ? JSON.generate(data) : '')
      return nil if optional && !status.success? && error.include?('(HTTP 404)')
      raise "Cannot access #{endpoint}: #{error.strip}" unless status.success?
      JSON.parse(out)
    end

    def initialize(repository)
      @repository = repository
      @base = "repos/#{repository}"
      release = api("#{@base}/releases/tags/#{POINTER}", optional: true)
      if release
        raise 'Invalid update-index release' unless release.fetch('tag_name') == POINTER &&
          release.fetch('prerelease') && !release.fetch('draft')
        @id = release.fetch('id')
        @state = JSON.parse(release.fetch('body'))
        @state.fetch('current')
      else
        @state = { 'current' => nil }
      end
    end

    def save(current:, pending: nil)
      state = { 'current' => current }
      state['pending'] = pending if pending
      body = { body: JSON.generate(state), make_latest: 'false' }
      if @id
        api("#{@base}/releases/#{@id}", method: 'PATCH', data: body)
      else
        release = api("#{@base}/releases", method: 'POST', data: body.merge(
          tag_name: POINTER, name: '更新通道索引', target_commitish: 'main', prerelease: true
        ))
        @id = release.fetch('id')
      end
      @state = state
    end

    def release(version)
      api("#{@base}/releases/tags/v#{ReleaseMetadata.version(version)}", optional: true)
    end

    def verify_assets(version, release, index:)
      tag = "v#{version}"
      raise "Release is not public or tag differs: #{tag}" unless release && release.fetch('tag_name') == tag &&
        !release.fetch('draft') && release.fetch('prerelease') == ReleaseMetadata.prerelease?(version)
      data = api("#{@base}/contents/release/platforms.yaml?ref=#{URI.encode_www_form_component(tag)}")
      platforms = YAML.safe_load(Base64.decode64(data.fetch('content'))).fetch('platforms')
      raise "Missing platforms for #{tag}" if platforms.empty?
      expected = platforms.map { |p| ReleaseMetadata.archive_name(version, p) }
      expected.concat(%w[checksums.txt compose.yaml config.example.yaml])
      expected << 'version.yaml' if index
      assets = release.fetch('assets').select { |a| a.fetch('state') == 'uploaded' && a.fetch('size').positive? }.to_h { |a| [a.fetch('name'), a] }
      missing = expected - assets.keys
      raise "Incomplete release #{tag}: #{missing.join(', ')}" unless missing.empty?
      assets
    end

    def download(version)
      content, error, status = Open3.capture3('gh', 'release', 'download', "v#{version}", '--repo', @repository,
        '--pattern', 'version.yaml', '--output', '-')
      raise "Cannot download v#{version} snapshot: #{error.strip}" unless status.success?
      content
    end

    def read_snapshot(version, asset)
      raise 'Invalid version.yaml size' unless (1..LIMIT).cover?(asset.fetch('size'))
      content = download(version)
      digest = Digest::SHA256.hexdigest(content)
      raise "Snapshot size or digest differs for v#{version}" unless content.bytesize == asset.fetch('size') && asset.fetch('digest') == "sha256:#{digest}"
      doc = YAML.safe_load(content)
      raise 'Invalid channel snapshot' unless doc.fetch('version') == version && doc.fetch('published').is_a?(Hash)
      [doc, digest]
    end

    def previous
      if (current = @state.fetch('current'))
        assets = verify_assets(current, release(current), index: true)
        return read_snapshot(current, assets.fetch('version.yaml')).first
      end
      # 首次接入时从实际公开的稳定版引导；没有历史 Release 的仓库从空表开始
      doc = { 'published' => {} }
      latest = api("#{@base}/releases/latest", optional: true)
      if latest
        version = ReleaseMetadata.version(latest.fetch('tag_name'))
        verify_assets(version, latest, index: false)
        PublishedChannels.advance(doc, version)
      end
      doc
    end

    # 调用方持有同一仓库的发布锁；快照必须在取锁后生成，不能在并行构建中预先合并
    def prepare(version, directory)
      if (pending = @state.dig('pending', 'version')) && (old = release(pending)) && !old.fetch('draft')
        finish(pending)
      end
      existing = release(version)
      raise "Release v#{version} already public; use index-only recovery" if existing && !existing.fetch('draft')
      doc = previous
      doc['version'] = version
      PublishedChannels.advance(doc, version)
      content = YAML.dump(doc)
      raise 'Published index exceeds the client size limit' if content.bytesize > LIMIT
      digest = Digest::SHA256.hexdigest(content)
      File.write(File.join(directory, 'version.yaml'), content)
      File.open(File.join(directory, 'checksums.txt'), 'a') { |file| file.puts "#{digest}  version.yaml" }
      save(current: @state.fetch('current'), pending: { 'version' => version, 'sha256' => digest })
    end

    def finish(version)
      assets = verify_assets(version, release(version), index: true)
      _, digest = read_snapshot(version, assets.fetch('version.yaml'))
      return if @state.fetch('current') == version
      unless @state['pending'] == { 'version' => version, 'sha256' => digest }
        raise "v#{version} is not the reserved snapshot; refusing to overwrite another publication"
      end
      save(current: version)
    end
  end
end

if $PROGRAM_NAME == __FILE__
  mode, raw = ARGV
  version = ReleaseMetadata.version(raw || '')
  publisher = PublishedChannels::Publisher.new(ENV.fetch('GITHUB_REPOSITORY'))
  case mode
  when 'prepare' then publisher.prepare(version, 'dist/release')
  when 'finish' then publisher.finish(version)
  else abort 'usage: published-channels.rb prepare|finish VERSION'
  end
end
