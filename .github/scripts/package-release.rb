require 'digest'
require 'fileutils'
require_relative 'release-metadata'

# 归档与镜像消费相同二进制和封口清单，不在打包时重新解释插件分发策略
module ReleasePackage
  APP_NAME = ReleaseMetadata::APP_NAME
  MANIFEST = 'dist/plugin-release-assets/plugin-release-manifest.json'

  def self.install_manifest(directory)
    manifest = JSON.parse(File.read(MANIFEST))
    unless manifest['schema_version'] == 1 && manifest['sealed'] == true && manifest['plugins'] == []
      raise 'Expected a sealed host manifest without bundled plugins'
    end
    FileUtils.mkdir_p(directory)
    destination = File.join(directory, File.basename(MANIFEST))
    FileUtils.cp(MANIFEST, destination)
    File.chmod(0o444, destination)
    File.chmod(0o555, directory)
  end

  def self.archives
    platforms = JSON.parse(ENV.fetch('PLATFORM_MATRIX')).fetch('include')
    release_dir = 'dist/release'
    FileUtils.rm_rf(release_dir)
    FileUtils.mkdir_p(release_dir)
    # 上一次打包生成的只读清单目录需要先恢复写权限
    FileUtils.chmod_R('u+w', 'package') if File.directory?('package')
    FileUtils.rm_rf('package')
    config = 'deploy/config.example.yaml'
    FileUtils.cp(config, release_dir)
    compose = File.read('deploy/compose.yaml')
    image = "#{ENV.fetch('REGISTRY')}/#{ENV.fetch('IMAGE_NAME')}"
    default_image = "${CPR_IMAGE:-#{image}:latest}"
    raise 'Expected exactly one default application image in deploy/compose.yaml' unless compose.scan(default_image).length == 1
    File.write("#{release_dir}/compose.yaml", compose.sub(default_image, "${CPR_IMAGE:-#{image}:#{ENV.fetch('DOCKER_VERSION')}}"))
    FileUtils.cp(MANIFEST, release_dir)

    platforms.each do |platform|
      raise "Unsupported archive format: #{platform['archive']}" unless platform['archive'] == 'tar.gz'
      name = "#{platform.fetch('os')}-#{platform.fetch('arch')}"
      binary = platform.fetch('binary')
      root = "package/#{name}/#{APP_NAME}"
      FileUtils.mkdir_p(["#{root}/web/dist", "#{root}/deploy"])
      FileUtils.cp("dist/binaries/#{APP_NAME}-#{name}/#{binary}", "#{root}/#{binary}")
      File.chmod(0o755, "#{root}/#{binary}")
      FileUtils.cp_r('dist/frontend/.', "#{root}/web/dist")
      FileUtils.cp(config, "#{root}/deploy")
      install_manifest("#{root}/plugins/official")
      archive = File.join(release_dir, ReleaseMetadata.archive_name(ENV.fetch('VERSION'), platform))
      raise "Failed to create #{archive}" unless system('tar', '-czf', archive, '-C', File.dirname(root), APP_NAME)
    end

    checksums = Dir.glob("#{release_dir}/*").sort.map { |path| "#{Digest::SHA256.file(path).hexdigest}  #{File.basename(path)}\n" }
    File.write("#{release_dir}/checksums.txt", checksums.join)
  end

  def self.image
    platform = "#{ENV.fetch('MATRIX_OS')}-#{ENV.fetch('MATRIX_ARCH')}"
    target = "dist/docker/#{platform}"
    FileUtils.mkdir_p(target)
    FileUtils.cp("dist/docker-artifacts/#{APP_NAME}-#{platform}/#{ENV.fetch('MATRIX_BINARY')}", "#{target}/#{APP_NAME}")
    File.chmod(0o755, "#{target}/#{APP_NAME}")
    install_manifest("#{target}/plugins/official")
  end
end

if $PROGRAM_NAME == __FILE__
  begin
    case ARGV
    when ['archives'] then ReleasePackage.archives
    when ['image'] then ReleasePackage.image
    else raise 'usage: package-release.rb archives|image'
    end
  rescue StandardError => error
    warn "error: #{error.message}"
    exit 1
  end
end
