require 'json'
require 'open3'

# 仅复用同一源码的主分支 CI，缺少证据时完整检查发行提交
module ReleaseQuality
  def self.capture(*command)
    output, error, status = Open3.capture3(*command)
    raise "#{command.first} failed: #{error.strip}" unless status.success?
    output
  end

  def self.evidence(repository)
    parents = capture('git', 'rev-list', '--parents', '-n', '1', 'HEAD').split
    return nil unless parents.length == 2
    parent = parents.last
    changes = capture('git', 'diff', '--name-only', '-z', parent, 'HEAD').split("\0")
    return nil if changes.empty? || (changes - %w[release/version.yaml release/notes.md]).any?

    # 按最新一次运行判断，失败或仍在执行时不回退到旧的成功记录
    endpoint = "repos/#{repository}/actions/workflows/ci.yml/runs?event=push&branch=main&head_sha=#{parent}&per_page=1"
    run = JSON.parse(capture('gh', 'api', endpoint)).fetch('workflow_runs').first
    return nil unless run && run.fetch('head_sha') == parent && run.fetch('conclusion') == 'success'
    return nil unless run.fetch('head_repository').fetch('full_name') == repository

    rows = capture('gh', 'api', '--paginate', "repos/#{repository}/actions/runs/#{run.fetch('id')}/jobs?per_page=100", '--jq', '.jobs[] | @json')
    job = rows.lines.map { |line| JSON.parse(line) }.find { |entry| entry.fetch('name') == 'quality / backend' }
    return nil unless job && job.fetch('conclusion') == 'success'
    job.fetch('html_url')
  end
end

if $PROGRAM_NAME == __FILE__
  begin
    evidence = ReleaseQuality.evidence(ENV.fetch('GITHUB_REPOSITORY'))
  rescue StandardError => error
    warn "CI evidence unavailable; running full release checks: #{error.message}"
    evidence = nil
  end
  puts "run_backend=#{evidence.nil?}"
  if ENV['GITHUB_STEP_SUMMARY']
    message = evidence ? "后端质量检查复用版本提交父提交的 [CI 结果](#{evidence})；仅版本与发布说明不同" : '未找到可复用的后端 CI 结果，完整检查发行提交'
    File.open(ENV.fetch('GITHUB_STEP_SUMMARY'), 'a') { |file| file.puts(message) }
  end
end
