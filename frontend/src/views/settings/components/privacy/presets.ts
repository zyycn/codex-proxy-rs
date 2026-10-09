import type { PrivacyAction, PrivacyRule, PrivacySample, PrivacyScope } from '@/api/modules/settings/privacy'

export const scopes: { value: PrivacyScope, label: string }[] = [
  { value: 'turn_metadata', label: '工作区附加信息' },
  { value: 'desktop_git_context', label: 'Desktop Git 信息' },
  { value: 'environment_text', label: '自动环境上下文' },
  { value: 'request_body', label: '指定请求字段' },
  { value: 'request_header', label: '指定请求头' },
]
export const actions: { value: PrivacyAction, label: string }[] = [
  { value: 'regex_replace', label: '正则替换' },
  { value: 'set_value', label: '固定值' },
  { value: 'rename_key', label: '字段名替换' },
  { value: 'remove_field', label: '删除字段 / 数组项' },
]
export const fieldsByScope: Partial<Record<PrivacyScope, { value: string, label: string }[]>> = {
  turn_metadata: [
    { value: '', label: '自定义字段路径' },
    { value: '$.workspaces.*.associated_remote_urls.*', label: 'Git 远端地址' },
    { value: '$.workspaces', label: '工作区路径键' },
    { value: '$.workspaces.*.latest_git_commit_hash', label: 'Git 提交标识' },
  ],
  desktop_git_context: [
    { value: '', label: '自定义字段路径' },
    { value: '$.remotes', label: 'Git 远端集合' },
    { value: '$.remotes[*].fetchUrl', label: 'Git 远端地址' },
    { value: '$.headCommitSha', label: 'Git 提交标识' },
    { value: '$.worktreeId', label: '工作区指纹' },
    { value: '$.appVersion', label: '应用版本' },
    { value: '$.appServerVersion', label: 'Core 版本' },
  ],
}

export function newRule(): PrivacyRule {
  return { id: Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, '0')).join(''), name: '', enabled: false, scope: 'turn_metadata', selector: '$.workspaces.*.associated_remote_urls.*', action: 'regex_replace', pattern: '', replacement: '', value: '', replaceAll: true, caseInsensitive: false, multiLine: false }
}

export const presets: { name: string, values: Partial<PrivacyRule> }[] = [
  { name: '工作区仓库地址脱敏', values: { scope: 'turn_metadata', selector: '$.workspaces.*.associated_remote_urls.*', action: 'regex_replace', pattern: '(?s)^.+$', replacement: '[REDACTED]' } },
  { name: '工作区用户名替换', values: { scope: 'turn_metadata', selector: '$.workspaces', action: 'rename_key', pattern: String.raw`(?i)^((?:[a-z]:)?[/\\](?:home|users)[/\\])[^/\\]+`, replacement: `\${1}user` } },
  { name: '移除工作区提交标识', values: { scope: 'turn_metadata', selector: '$.workspaces.*.latest_git_commit_hash', action: 'remove_field', pattern: null } },
  // Desktop Git 的这些字段可能为 null，按字段移除，避免正则因类型不匹配撤销整条规则
  // remotes 本身也可能为 null，预设处理整个远端集合，单个地址仍可从常用字段选择
  { name: '移除 Desktop Git 远端', values: { scope: 'desktop_git_context', selector: '$.remotes', action: 'remove_field', pattern: null } },
  { name: '移除 Desktop 提交标识', values: { scope: 'desktop_git_context', selector: '$.headCommitSha', action: 'remove_field', pattern: null } },
  { name: '移除 Desktop 工作区指纹', values: { scope: 'desktop_git_context', selector: '$.worktreeId', action: 'remove_field', pattern: null } },
  { name: '移除 Desktop 应用版本', values: { scope: 'desktop_git_context', selector: '$.appVersion', action: 'remove_field', pattern: null } },
  { name: '移除 Desktop Core 版本', values: { scope: 'desktop_git_context', selector: '$.appServerVersion', action: 'remove_field', pattern: null } },
]

// 样本使用虚构仓库、路径与标识，字段形状对应官方 Core 和 Desktop Git 上下文
const metadata = { workspaces: { '/home/alex/projects/demo': { associated_remote_urls: { origin: 'https://git.example.invalid/private-org/repo.git', public: 'git@git.example.invalid:public/repo.git' }, latest_git_commit_hash: 'a'.repeat(40), has_changes: true } }, request_kind: 'turn' }
const desktopGit = {
  schemaVersion: 1,
  phase: 'before_turn_request',
  hostId: 'local',
  sessionId: 'example-session',
  threadId: 'example-thread',
  clientUserMessageId: 'example-message',
  appVersion: '26.1007.21434',
  appServerVersion: '0.162.0-alpha.17.2',
  status: 'partial',
  capturedAt: '2026-10-09T12:00:00Z',
  worktreeId: 'b'.repeat(40),
  headCommitSha: 'a'.repeat(40),
  branch: 'main',
  detached: false,
  upstream: { remoteName: 'origin', mergeRef: 'refs/heads/main', trackingRef: 'refs/remotes/origin/main' },
  remotes: [
    { name: 'origin', fetchUrl: 'https://git.example.invalid/private-org/repo.git' },
    { name: 'upstream', fetchUrl: 'ssh://git.example.invalid/public/repo.git' },
    { name: 'unavailable', fetchUrl: null, unavailableReason: 'unsafe_url' },
  ],
  remotesOmitted: 0,
}

function desktopContext(value: unknown): string {
  const text = JSON.stringify(value).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
  return `<external_codex_git_state><environment_context><git source="app" trust="untrusted">${text}</git></environment_context></external_codex_git_state>`
}

function contextMessage(text: string) {
  return { role: 'user', content: [{ type: 'input_text', text }] }
}

export function sampleText(scope: PrivacyScope): string {
  if (scope === 'turn_metadata')
    return JSON.stringify(metadata, null, 2)
  if (scope === 'desktop_git_context')
    return JSON.stringify(desktopGit, null, 2)
  if (scope === 'environment_text')
    return '<environment_context>\n<cwd>/home/alex/projects/demo</cwd>\n<timezone>Asia/Shanghai</timezone>\n</environment_context>'
  if (scope === 'request_header')
    return 'example-client'
  return JSON.stringify({ metadata: { workspace: '/home/alex/projects/demo' } }, null, 2)
}

export function makeSample(scope: PrivacyScope, text: string, selector = ''): PrivacySample {
  const sample: PrivacySample = { body: {}, headers: {}, turnMetadata: null }
  if (scope === 'turn_metadata') {
    sample.turnMetadata = JSON.stringify(JSON.parse(text))
  }
  else if (scope === 'request_header') {
    sample.headers[selector] = [text]
  }
  else if (scope === 'request_body') {
    sample.body = JSON.parse(text)
  }
  else {
    const context = scope === 'desktop_git_context'
      ? desktopContext(JSON.parse(text))
      : text
    sample.body = { input: [contextMessage(context)] }
  }
  return sample
}

export function exampleRequest(): PrivacySample {
  return { body: { input: [contextMessage(desktopContext(desktopGit))], client_metadata: { 'x-codex-turn-metadata': JSON.stringify(metadata) } }, headers: { 'x-codex-turn-metadata': [JSON.stringify(metadata)] }, turnMetadata: null }
}
