import type { ProviderRequestProfiles } from '@/api'

export function isValidInstallationSeed(value: unknown) {
  return value == null || value === '' || (typeof value === 'string' && value.length === 36 && /^[\da-f]{8}(?:-[\da-f]{4}){3}-[\da-f]{12}$/i.test(value))
}

export function decodeKeyProfiles(profiles: ProviderRequestProfiles) {
  const { openai, ...others } = profiles
  if (!openai)
    return { profiles: others, installationSeed: '' }

  if (openai.inherit && typeof openai.inherit === 'object') {
    const seed = 'installationSeed' in openai.inherit && openai.inherit.installationSeed
    return {
      profiles: others,
      installationSeed: typeof seed === 'string'
        ? seed
        : '',
    }
  }

  const { installationSeed, ...configuration } = openai
  return {
    profiles: { ...others, openai: configuration },
    installationSeed: typeof installationSeed === 'string'
      ? installationSeed
      : '',
  }
}

export function encodeKeyProfiles(profiles: ProviderRequestProfiles, installationSeed: string): ProviderRequestProfiles {
  if (!installationSeed)
    return profiles
  return {
    ...profiles,
    openai: profiles.openai
      ? { ...profiles.openai, installationSeed }
      : {
          inherit: { installationSeed },
        },
  }
}
