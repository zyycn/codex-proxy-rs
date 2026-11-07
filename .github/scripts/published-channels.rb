require 'base64'
require 'uri'
require_relative 'release-metadata'

# 各发行线共用 main 通道表；只合并本次已核实的版本
module PublishedChannels
  PATH = 'release/version.yaml'
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
      @base = "repos/#{repository}"
    end

    def find_release(version)
      api("#{@base}/releases/tags/v#{ReleaseMetadata.version(version)}", optional: true)
    end

    def verify_assets(version)
      release = find_release(version)
      tag = "v#{version}"
      raise "Release is not public or tag differs: #{tag}" unless release && release.fetch('tag_name') == tag &&
        !release.fetch('draft') && release.fetch('prerelease') == ReleaseMetadata.prerelease?(version)
      data = api("#{@base}/contents/release/platforms.yaml?ref=#{URI.encode_www_form_component(tag)}")
      platforms = YAML.safe_load(Base64.decode64(data.fetch('content'))).fetch('platforms')
      raise "Missing platforms for #{tag}" if platforms.empty?
      expected = platforms.map { |p| ReleaseMetadata.archive_name(version, p) }
      expected.concat(%w[checksums.txt compose.yaml config.example.yaml])
      uploaded = release.fetch('assets').filter_map do |asset|
        asset.fetch('name') if asset.fetch('state') == 'uploaded' && asset.fetch('size').positive?
      end
      missing = expected - uploaded
      raise "Incomplete release #{tag}: #{missing.join(', ')}" unless missing.empty?
    end

    def check_unpublished(version)
      existing = find_release(version)
      raise "Release v#{version} already public; use index-only recovery" if existing && !existing.fetch('draft')
    end

    def publish(version)
      verify_assets(version)
      # 读取 main 的当前文件，实验分支和较晚结束的旧构建都不能覆盖其他通道或构建版本
      endpoint = "#{@base}/contents/#{PATH}"
      file = api("#{endpoint}?ref=main")
      document = YAML.safe_load(Base64.decode64(file.fetch('content')))
      return unless PublishedChannels.advance(document, version)

      content = YAML.dump(document).delete_prefix("---\n")
      raise 'Published index exceeds the client size limit' if content.bytesize > LIMIT
      # 按读取的文件 SHA 写入；并发备版改过同一文件时交给恢复任务重新合并
      api(endpoint, method: 'PUT', data: {
        branch: 'main', sha: file.fetch('sha'), content: Base64.strict_encode64(content),
        message: "chore(release): update release channels to #{version}"
      })
    end
  end
end

if $PROGRAM_NAME == __FILE__
  mode, raw = ARGV
  version = ReleaseMetadata.version(raw || '')
  publisher = PublishedChannels::Publisher.new(ENV.fetch('GITHUB_REPOSITORY'))
  case mode
  when 'check' then publisher.check_unpublished(version)
  when 'publish' then publisher.publish(version)
  else abort 'usage: published-channels.rb check|publish VERSION'
  end
end
