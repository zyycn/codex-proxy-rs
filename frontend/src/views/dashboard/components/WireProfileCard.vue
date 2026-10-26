<script setup lang="ts">
import type { DashboardWireProfile } from '@/api/modules/dashboard'
import { ZCard, ZEmpty, ZSegmented } from '@codex-proxy/ui'

import { Box, CheckCircle2, Monitor, RefreshCw, ShieldCheck, Terminal, TriangleAlert } from '@lucide/vue'

import { computed, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { formatProviderLabel, isSupportedProvider, providerIcon } from '@/utils/providers'

const props = defineProps<{
  profiles: DashboardWireProfile[]
}>()

const { t } = useI18n({ useScope: 'global' })

const activeProvider = shallowRef('')

const providerOptions = computed(() =>
  props.profiles.map(profile => ({
    label: formatProviderLabel(profile.provider),
    value: profile.provider,
    icon: providerIcon(profile.provider),
  })),
)

const profile = computed(
  () => props.profiles.find(item => item.provider === activeProvider.value) ?? props.profiles[0] ?? null,
)

const versionParts = computed(() => {
  const version = profile.value?.version ?? ''
  const separator = version.indexOf('-')
  if (separator <= 0 || separator === version.length - 1)
    return { release: version, prerelease: '' }

  return {
    release: version.slice(0, separator),
    prerelease: version.slice(separator + 1),
  }
})

const releaseLabel = computed(() => {
  const release = profile.value?.release
  if (!release?.latestVersion)
    return t('dashboard.clientIdentity.notChecked')
  return release.latestBuild
    ? `${release.latestVersion} · Build ${release.latestBuild}`
    : release.latestVersion
})

const releaseStatus = computed(() => {
  const current = profile.value
  if (!current?.release) {
    return {
      label: current?.attributes.some(item => item.key === 'version_policy' && item.value === 'fixed')
        ? t('dashboard.clientIdentity.customVersion')
        : t('dashboard.clientIdentity.active'),
      title: t('dashboard.clientIdentity.activeDescription'),
      tone: 'bg-cp-info-container text-cp-info-on-container',
      icon: ShieldCheck,
    }
  }

  const status = current.release.status ?? 'unchecked'
  if (status === 'aligned') {
    return {
      label: t('dashboard.clientIdentity.upToDate'),
      title: t('dashboard.clientIdentity.upToDateDescription'),
      tone: 'bg-cp-success-container text-cp-success-on-container',
      icon: CheckCircle2,
    }
  }
  if (status === 'review_required') {
    return {
      label: t('dashboard.clientIdentity.updateAvailable'),
      title: t('dashboard.clientIdentity.latestRelease', { releaseLabel: releaseLabel.value }),
      tone: 'bg-cp-warning-container text-cp-warning-on-container',
      icon: TriangleAlert,
    }
  }
  if (status === 'check_failed') {
    return {
      label: t('dashboard.clientIdentity.checkFailed'),
      title: current.release.error || t('dashboard.clientIdentity.checkFailedDescription'),
      tone: 'bg-cp-error-container text-cp-error-on-container',
      icon: TriangleAlert,
    }
  }
  return {
    label: t('dashboard.clientIdentity.pending'),
    title: t('dashboard.clientIdentity.pendingDescription'),
    tone: 'bg-cp-cyan-container text-cp-cyan-on-container',
    icon: RefreshCw,
  }
})

const verifiedLabel = computed(() => {
  const verifiedAt = profile.value?.verifiedAtDisplay
  return verifiedAt
    ? t('dashboard.clientIdentity.verifiedAt', { verifiedAt })
    : undefined
})

const checkedLabel = computed(() => {
  const checkedAt = profile.value?.release?.checkedAtDisplay
  return checkedAt
    ? t('dashboard.clientIdentity.checkedAt', { checkedAt })
    : undefined
})

function toPascalCase(value: string) {
  return value
    .split(/[^a-z0-9]+/i)
    .filter(Boolean)
    .map(part => `${part.charAt(0).toUpperCase()}${part.slice(1).toLowerCase()}`)
    .join('')
}

const clientIdentity = computed(() => {
  const current = profile.value
  if (!current)
    return '—'
  const value = current.attributes.find(attribute => attribute.key === 'client_identifier')?.value
  if (!value)
    return '—'
  return current.provider === 'xai'
    ? toPascalCase(value)
    : value
})

const attributeLabels = computed<Record<string, string>>(() => ({
  client_identifier: t('dashboard.clientIdentity.clientId'),
  token_auth: t('dashboard.clientIdentity.authProtocol'),
  client_mode: t('dashboard.clientIdentity.clientMode'),
  version_policy: t('dashboard.clientIdentity.versionPolicy'),
}))

const identityAttributes = computed(() => {
  const current = profile.value
  if (!current)
    return []
  return isSupportedProvider(current.provider)
    ? [
        {
          label: t('dashboard.clientIdentity.clientId'),
          value: clientIdentity.value,
        },
      ]
    : current.attributes.map(attribute => ({
        label: attributeLabels.value[attribute.key] ?? attribute.key,
        value: attribute.value,
      }))
})

const authProtocol = computed(() => {
  const value = profile.value?.attributes.find(attribute => attribute.key === 'token_auth')?.value
  if (!value)
    return '—'
  return toPascalCase(value)
})

const runtimeEnvironment = computed(() => {
  const target = profile.value?.target
  if (!target)
    return { primary: '—', details: [] as string[], title: '—' }

  const present = (value: string) => value !== '—' && value.toLowerCase() !== 'unknown'
  const primary = [target.osType, target.osVersion].filter(present).join(' ')
  const details = [target.arch, target.terminal].filter(present)
  return {
    primary: primary || '—',
    details,
    title: [primary, ...details].filter(Boolean).join(' · '),
  }
})

watch(
  () => props.profiles,
  (profiles) => {
    if (!profiles.some(item => item.provider === activeProvider.value))
      activeProvider.value = profiles[0]?.provider ?? ''
  },
  { immediate: true },
)
</script>

<template>
  <ZCard as="article" class="flex min-h-95 w-full flex-col">
    <template #header>
      <div class="flex min-w-0 flex-wrap items-start justify-between gap-3">
        <h2 class="m-0 pt-0.5 text-xl leading-[1.15] font-heavy text-cp-text text-balance">
          {{ t('dashboard.clientIdentity.title') }}
        </h2>
        <div v-if="providerOptions.length > 1" class="min-w-0 max-w-full overflow-x-auto">
          <ZSegmented
            v-model="activeProvider"
            :aria-label="t('dashboard.clientIdentity.provider')"
            :options="providerOptions"
            :style="{ width: `${providerOptions.length * 40 + 4}px` }"
            display="icon"
          />
        </div>
      </div>
    </template>

    <template #body>
      <ZEmpty v-if="!profile" :title="t('dashboard.clientIdentity.empty')" surface="inset" class="min-h-71.75 flex-1 place-content-center" />

      <div v-else class="flex flex-1">
        <section
          :aria-label="t('dashboard.clientIdentity.attributes')"
          class="grid min-w-0 flex-1 content-between gap-6 rounded-cp-lg bg-cp-fill-alter/70 px-5 py-5.5 sm:px-6 sm:py-5"
          :class="
            verifiedLabel || checkedLabel
              ? 'sm:grid-rows-[auto_minmax(0,1fr)_auto_auto]'
              : 'sm:grid-rows-[auto_minmax(0,1fr)_auto]'
          "
        >
          <div class="flex min-w-0 items-center justify-between gap-3">
            <div class="flex min-w-0 items-center gap-2 text-cp-text">
              <span class="inline-flex size-7 shrink-0 items-center justify-center rounded-lg bg-cp-fill-tertiary">
                <Box class="size-3.75 text-cp-text-secondary" />
              </span>
              <span class="truncate text-cp-xs leading-none font-heavy" :title="profile.product">{{ profile.product }}</span>
            </div>
            <span
              class="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-lg px-2.5 text-cp-sm leading-none font-bold"
              :class="releaseStatus.tone"
              :title="releaseStatus.title"
            >
              <component :is="releaseStatus.icon" class="size-3.5" />
              {{ releaseStatus.label }}
            </span>
          </div>

          <div class="grid min-h-0 content-center">
            <div class="flex min-w-0 flex-wrap items-baseline gap-x-2.5 gap-y-1.5">
              <strong
                :aria-label="profile.version"
                class="inline-flex max-w-full min-w-0 items-baseline gap-2 font-mono leading-none tabular-nums"
                :title="profile.version"
              >
                <span class="wrap-break-word text-[27px] leading-[1.05] font-heavy text-cp-text">
                  {{ versionParts.release }}
                </span>
                <span v-if="versionParts.prerelease" class="truncate text-cp-sm font-bold text-cp-text-secondary">
                  {{ versionParts.prerelease }}
                </span>
              </strong>
              <span
                v-if="profile.build"
                class="shrink-0 font-mono text-[10px] leading-none font-emphasis tabular-nums text-cp-text-quaternary"
              >
                {{ t('common.fields.build') }} {{ profile.build }}
              </span>
            </div>
          </div>

          <dl class="m-0 flex min-w-0 flex-wrap gap-x-7 gap-y-5">
            <div v-if="profile.provider === 'xai'" class="min-w-0 flex-auto">
              <dt class="flex items-center gap-1.5 text-[10px] leading-none font-bold text-cp-text-quaternary">
                <ShieldCheck class="size-3.25 text-cp-text-tertiary" />
                {{ t('dashboard.clientIdentity.authProtocol') }}
              </dt>
              <dd
                class="mt-2 mb-0 font-mono text-cp-lg leading-snug font-bold wrap-anywhere tabular-nums text-cp-text"
                :title="authProtocol"
              >
                {{ authProtocol }}
              </dd>
            </div>

            <div class="min-w-0 flex-auto">
              <dt class="flex items-center gap-1.5 text-[10px] leading-none font-bold text-cp-text-quaternary">
                <Monitor class="size-3.25 text-cp-text-tertiary" />
                {{ profile.provider === 'openai' ? t('dashboard.clientIdentity.emulatedEnvironment') : t('dashboard.clientIdentity.environment') }}
              </dt>
              <dd
                class="mt-2 mb-0 font-mono text-cp-lg leading-snug font-bold wrap-anywhere tabular-nums text-cp-text"
                :title="runtimeEnvironment.title"
              >
                {{ runtimeEnvironment.primary }}
                <span v-if="runtimeEnvironment.details.length" class="text-cp-xs font-emphasis text-cp-text-secondary">
                  <template v-for="detail in runtimeEnvironment.details" :key="detail"> · {{ detail }} </template>
                </span>
              </dd>
            </div>

            <div v-for="(attribute, index) in identityAttributes" :key="index" class="min-w-0 max-w-full">
              <dt class="flex items-center gap-1.5 text-[10px] leading-none font-bold text-cp-text-quaternary">
                <Terminal class="size-3.25 text-cp-text-tertiary" />
                {{ attribute.label }}
              </dt>
              <dd
                class="mt-2 mb-0 font-mono text-[16px] leading-snug font-heavy wrap-anywhere tabular-nums text-cp-text"
                :title="attribute.value"
              >
                {{ attribute.value }}
              </dd>
            </div>
          </dl>

          <footer
            v-if="verifiedLabel || checkedLabel"
            class="flex min-w-0 flex-wrap items-center justify-between gap-x-4 gap-y-1 text-[10px] leading-none font-emphasis text-cp-text-quaternary"
          >
            <span v-if="verifiedLabel" :title="profile.userAgent">{{ verifiedLabel }}</span>
            <span v-if="checkedLabel">{{ checkedLabel }}</span>
          </footer>
        </section>
      </div>
    </template>
  </ZCard>
</template>
