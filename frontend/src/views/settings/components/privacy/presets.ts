import type { PrivacyAction, PrivacyRule, PrivacySample, PrivacyScope, PrivacyStep } from '@/api/modules/settings/privacy'
import { cloneDeep } from 'es-toolkit'
import { computed } from 'vue'
import { t } from '@/i18n'

export const scopes = computed<{ value: PrivacyScope, label: string }[]>(() => ([
  {
    value: 'turn_metadata',
    label: t('settings.privacy.presets.workspaceMetadata'),
  },
  {
    value: 'desktop_git_context',
    label: t('settings.privacy.presets.desktopGitInformation'),
  },
  {
    value: 'environment_text',
    label: t('settings.privacy.presets.automaticEnvironmentContext'),
  },
  {
    value: 'request_body',
    label: t('settings.privacy.presets.specifyRequestFields'),
  },
  {
    value: 'request_header',
    label: t('settings.privacy.presets.specifyRequestHeader'),
  },
]))
export const actions = computed<{ value: PrivacyAction, label: string }[]>(() => ([
  {
    value: 'regex_replace',
    label: t('settings.privacy.presets.regularReplacement'),
  },
  {
    value: 'set_value',
    label: t('settings.privacy.presets.fixedValue'),
  },
  {
    value: 'rename_key',
    label: t('settings.privacy.presets.fieldNameReplacement'),
  },
  {
    value: 'remove_field',
    label: t('settings.privacy.presets.deleteFieldArrayItem'),
  },
]))
export const fieldsByScope = computed<Partial<Record<PrivacyScope, { value: string, label: string }[]>>>(() => ({
  turn_metadata: [
    {
      value: '',
      label: t('settings.privacy.presets.customFieldPath'),
    },
    {
      value: '$.workspaces.*.associated_remote_urls.*',
      label: t('settings.privacy.presets.gitRemoteAddress'),
    },
    {
      value: '$.workspaces',
      label: t('settings.privacy.presets.workspacePathKey'),
    },
    {
      value: '$.workspaces.*.latest_git_commit_hash',
      label: t('settings.privacy.presets.gitCommitId'),
    },
  ],
  desktop_git_context: [
    {
      value: '',
      label: t('settings.privacy.presets.customFieldPath'),
    },
    {
      value: '$.remotes',
      label: t('settings.privacy.presets.gitRemoteCollection'),
    },
    {
      value: '$.remotes[*].fetchUrl',
      label: t('settings.privacy.presets.gitRemoteAddress'),
    },
    {
      value: '$.headCommitSha',
      label: t('settings.privacy.presets.gitCommitId'),
    },
    {
      value: '$.worktreeId',
      label: t('settings.privacy.presets.workspaceFingerprint'),
    },
  ],
}))

export function newStep(): PrivacyStep {
  return {
    scope: 'turn_metadata',
    selector: '$.workspaces.*.associated_remote_urls.*',
    action: 'regex_replace',
    pattern: '',
    replacement: '',
    value: '',
    replaceAll: true,
    caseInsensitive: false,
    multiLine: false,
  }
}

export function newRule(): PrivacyRule {
  return {
    id: Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, '0')).join(''),
    name: '',
    enabled: false,
    steps: [
      newStep(),
    ],
  }
}

export const presets = computed<{ key: string, name: string, steps: Partial<PrivacyStep>[] }[]>(() => ([
  {
    key: 'workspace',
    name: t('settings.privacy.presets.unifyWorkspaceInformation'),
    steps: [
      // 只改写附加元数据中的路径，正文路径供本地工具执行使用
      {
        scope: 'turn_metadata',
        selector: '$.workspaces',
        action: 'rename_key',
        pattern: String.raw`(?i)^((?:[a-z]:)?[/\\](?:home|users)[/\\])[^/\\]+`,
        replacement: `\${1}user`,
      },
      {
        scope: 'turn_metadata',
        selector: '$.workspaces.*.associated_remote_urls.*',
        action: 'regex_replace',
        pattern: String.raw`(?i)^((?:(?:https?|ssh|git)://[^/?#]+/|[^/@:\s]+@[^/:\s]+:))[^/?#]+/([^/?#]+)(/?)$`,
        replacement: `\${1}alex/\${2}\${3}`,
      },
    ],
  },
]))

export function createPreset(key: string): PrivacyRule {
  const preset = presets.value.find(item => item.key === key)!
  return {
    ...newRule(),
    name: preset.name,
    steps: preset.steps.map(step => ({
      ...newStep(),
      ...cloneDeep(step),
    })),
  }
}

// 样本使用虚构仓库、路径与标识，字段形状对应官方 Core 和 Desktop Git 上下文
const metadata = {
  workspaces: {
    '/home/alex/projects/demo': {
      associated_remote_urls: { origin: 'https://git.example.invalid/private-org/repo.git', public: 'git@git.example.invalid:public/repo.git' },
      latest_git_commit_hash: 'a'.repeat(40),
      has_changes: true,
    },
  },
  request_kind: 'turn',
}
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
  return {
    role: 'user',
    content: [
      { type: 'input_text', text },
    ],
  }
}

export function exampleRequest(): PrivacySample {
  const environment = '<environment_context>\n<cwd>/home/alex/projects/demo</cwd>\n<filesystem><workspace_roots><root>/home/alex/projects/demo</root></workspace_roots></filesystem>\n<timezone>Asia/Shanghai</timezone>\n</environment_context>'
  return {
    body: {
      input: [
        contextMessage(environment),
        contextMessage(desktopContext(desktopGit)),
      ],
      client_metadata: {
        'x-codex-turn-metadata': JSON.stringify(metadata),
      },
    },
    headers: {
      'x-codex-turn-metadata': [
        JSON.stringify(metadata),
      ],
    },
    turnMetadata: null,
  }
}
