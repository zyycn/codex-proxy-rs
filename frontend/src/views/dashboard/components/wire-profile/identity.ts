import type { DashboardWireProfile } from '@/api'
import type { BuiltinProvider } from '@/providers/catalog'
import { t } from '@/i18n'
import { isBuiltinProvider } from '@/providers/catalog'

function pascalCase(value: string) {
  return value.split(/[^a-z0-9]+/i).filter(Boolean).map(part => part.charAt(0).toUpperCase() + part.slice(1).toLowerCase()).join('')
}

const labels: Record<string, () => string> = {
  client_identifier: () => t('dashboard.clientIdentity.clientId'),
  token_auth: () => t('dashboard.clientIdentity.authProtocol'),
  client_mode: () => t('dashboard.clientIdentity.clientMode'),
  version_policy: () => t('dashboard.clientIdentity.versionPolicy'),
}

interface IdentityPresentation {
  identifier: (value: string) => string
  emulatedEnvironment: boolean
  versionOnly: boolean
}

const presentations: Record<BuiltinProvider, IdentityPresentation> = {
  openai: {
    identifier: value => value,
    emulatedEnvironment: true,
    versionOnly: false,
  },
  xai: {
    identifier: pascalCase,
    emulatedEnvironment: false,
    versionOnly: false,
  },
  antigravity: {
    identifier: value => value.replace(/^antigravity\/hub\//, 'Antigravity/Hub/').replace(/^antigravity\/cli\//, 'Antigravity/CLI/'),
    emulatedEnvironment: true,
    versionOnly: true,
  },
}

export function describeWireProfile({ provider, attributes }: DashboardWireProfile) {
  const presentation = isBuiltinProvider(provider)
    ? presentations[provider]
    : undefined
  const client = attributes.find(item => item.key === 'client_identifier')?.value
  const auth = attributes.find(item => item.key === 'token_auth')?.value
  return {
    environmentLabel: presentation?.emulatedEnvironment
      ? t('dashboard.clientIdentity.emulatedEnvironment')
      : t('dashboard.clientIdentity.environment'),
    alignedLabel: presentation?.versionOnly
      ? t('dashboard.clientIdentity.versionUpToDate')
      : t('dashboard.clientIdentity.upToDate'),
    authProtocol: presentation && auth
      ? pascalCase(auth)
      : undefined,
    attributes: presentation
      ? [
          {
            label: t('dashboard.clientIdentity.clientId'),
            value: client
              ? presentation.identifier(client)
              : '—',
          },
        ]
      : attributes.map(({ key, value }) => ({
          label: Object.hasOwn(labels, key)
            ? labels[key]!()
            : key,
          value,
        })),
  }
}
