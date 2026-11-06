require 'tmpdir'
require_relative '../published-channels'

# 测试只替换 GitHub 读写边界，执行真实的发布状态转换
class PublishedChannelsTest
  def assert_equal(expected, actual)
    raise "expected #{expected.inspect}, got #{actual.inspect}" unless expected == actual
  end

  def assert_rejected
    yield
  rescue RuntimeError
    return
  else
    raise 'expected publication to be rejected'
  end

  def initialize
    @pointer = nil
    @releases = {}
    @files = {}
    @calls = []
    @platforms = [{ 'os' => 'linux', 'arch' => 'amd64', 'archive' => 'tar.gz' }]
  end

  def api(endpoint, method: 'GET', data: nil, optional: false)
    path = endpoint.delete_prefix('repos/test/repo/')
    @calls << [method, path]
    case [method, path]
    when ['GET', 'releases/tags/update-index'] then @pointer
    when ['POST', 'releases']
      raise 'duplicate pointer' if @pointer
      @pointer = data.transform_keys(&:to_s).merge('id' => 12, 'draft' => false)
    when ['PATCH', 'releases/12']
      @pointer.merge!(data.transform_keys(&:to_s))
    when ['GET', 'releases/latest'] then @releases['3.41.2']
    else
      return @releases[path.delete_prefix('releases/tags/v')] if method == 'GET' && path.start_with?('releases/tags/v')
      return { 'content' => Base64.encode64(YAML.dump({ 'platforms' => @platforms })) } if path.start_with?('contents/release/platforms.yaml?ref=v')
      raise "unexpected #{method} #{path}"
    end
  end

  def publish_assets(version, content = nil)
    names = %w[checksums.txt compose.yaml config.example.yaml plugin-release-manifest.json]
    names << ReleaseMetadata.archive_name(version, @platforms.first)
    @files[version] = content if content
    assets = names.map { |name| { 'name' => name, 'size' => 100, 'state' => 'uploaded' } }
    assets << { 'name' => 'version.yaml', 'size' => content.bytesize, 'digest' => "sha256:#{Digest::SHA256.hexdigest(content)}", 'state' => 'uploaded' } if content
    @releases[version] = { 'tag_name' => "v#{version}", 'draft' => false, 'prerelease' => ReleaseMetadata.prerelease?(version), 'assets' => assets }
  end

  def publisher
    github = self
    files = @files
    Class.new(PublishedChannels::Publisher) do
      define_method(:api) { |endpoint, **args| github.api(endpoint, **args) }
      define_method(:download) { |version| files.fetch(version) }
    end.new('test/repo')
  end

  def prepare(version)
    Dir.mktmpdir do |dir|
      File.write(File.join(dir, 'checksums.txt'), '')
      publisher.prepare(version, dir)
      content = File.read(File.join(dir, 'version.yaml'))
      assert_equal "#{Digest::SHA256.hexdigest(content)}  version.yaml\n", File.read(File.join(dir, 'checksums.txt'))
      content
    end
  end

  def current = JSON.parse(@pointer.fetch('body')).fetch('current')

  def test_interrupted_publication_recovers_without_losing_another_channel
    publish_assets('3.41.2')
    stable = prepare('3.42.0')
    assert_equal nil, current
    assert_rejected { publisher.finish('3.42.0') }
    publish_assets('3.42.0', stable)
    # 模拟公开后更新指针中断，下一次发布在同一把锁内补写，再合并自己的通道
    beta = prepare('3.43.0-beta.1')
    assert_equal '3.42.0', current
    assert_equal({ 'stable' => '3.42.0', 'rc' => '3.42.0', 'beta' => '3.43.0-beta.1', 'alpha' => '3.43.0-beta.1' },
      YAML.safe_load(beta).dig('published', '3'))
    publish_assets('3.43.0-beta.1', beta)
    publisher.finish('3.43.0-beta.1')
    publisher.finish('3.43.0-beta.1')
    assert_rejected { publisher.finish('3.42.0') }
    assert_equal '3.43.0-beta.1', current
    experiment = YAML.safe_load(prepare('3.44.0-exp.1')).dig('published', '3')
    assert_equal({ '3.44.0' => '3.44.0-exp.1' }, experiment.fetch('exp'))
    assert_equal '3.43.0-beta.1', experiment.fetch('beta')
    assert_equal false, @calls.include?(['GET', 'releases'])
  end

  def test_incomplete_corrupt_or_draft_release_keeps_last_public_snapshot
    publish_assets('3.42.0', prepare('3.42.0'))
    publisher.finish('3.42.0')
    beta = prepare('3.43.0-beta.1')
    publish_assets('3.43.0-beta.1', beta)
    release = @releases.fetch('3.43.0-beta.1')
    release['draft'] = true
    assert_rejected { publisher.finish('3.43.0-beta.1') }
    release['draft'] = false
    missing = release.fetch('assets').shift
    assert_rejected { publisher.finish('3.43.0-beta.1') }
    release.fetch('assets') << missing
    publish_assets('3.43.0-beta.1', beta.sub('3.42.0', '3.10.0'))
    assert_rejected { publisher.finish('3.43.0-beta.1') }
    assert_equal '3.42.0', current
  end
end
PublishedChannelsTest.new.test_interrupted_publication_recovers_without_losing_another_channel
PublishedChannelsTest.new.test_incomplete_corrupt_or_draft_release_keeps_last_public_snapshot
puts 'Published channel recovery and integrity checks passed'
