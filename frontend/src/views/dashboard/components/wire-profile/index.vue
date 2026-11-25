<script setup lang="ts">
import type { DashboardWireProfile } from '@/api'
import { ZCard, ZEmpty, ZSegmented } from '@codex-proxy/ui'

import { Box, CheckCircle2, Monitor, RefreshCw, ShieldCheck, Terminal, TriangleAlert } from '@lucide/vue'

import { computed, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { formatProviderLabel, PROVIDER_IDS, providerIcon } from '@/providers/catalog'
import { describeWireProfile } from './identity'

const props = defineProps<{
  profiles: DashboardWireProfile[]
}>()

const { t } = useI18n({ useScope: 'global' })

const selectedProvider = shallowRef('')

const providerOptions = computed(() => {
  const providers = new Set([
    ...PROVIDER_IDS,
    ...props.profiles.map(profile => profile.provider),
  ])
  return Array.from(providers, value => ({
    label: formatProviderLabel(value),
    value,
    icon: providerIcon(value),
  }))
})

const activeProvider = computed({
  get: () => providerOptions.value.some(option => option.value === selectedProvider.value)
    ? selectedProvider.value
    : providerOptions.value[0]?.value ?? '',
  set: (value: string) => { selectedProvider.value = value },
})

const profile = computed(
  () => props.profiles.find(item => item.provider === activeProvider.value),
)

const identity = computed(() => profile.value && describeWireProfile(profile.value))

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
      tone: 'bg-z-info-container text-z-info-on-container',
      icon: ShieldCheck,
    }
  }

  const status = current.release.status ?? 'unchecked'
  if (status === 'aligned') {
    return {
      label: identity.value?.alignedLabel,
      title: t('dashboard.clientIdentity.upToDateDescription'),
      tone: 'bg-z-success-container text-z-success-on-container',
      icon: CheckCircle2,
    }
  }
  if (status === 'review_required') {
    return {
      label: t('dashboard.clientIdentity.updateAvailable'),
      title: t('dashboard.clientIdentity.latestRelease', { releaseLabel: releaseLabel.value }),
      tone: 'bg-z-warning-container text-z-warning-on-container',
      icon: TriangleAlert,
    }
  }
  if (status === 'check_failed') {
    return {
      label: t('dashboard.clientIdentity.checkFailed'),
      title: current.release.error || t('dashboard.clientIdentity.checkFailedDescription'),
      tone: 'bg-z-error-container text-z-error-on-container',
      icon: TriangleAlert,
    }
  }
  return {
    label: t('dashboard.clientIdentity.pending'),
    title: t('dashboard.clientIdentity.pendingDescription'),
    tone: 'bg-z-cyan-container text-z-cyan-on-container',
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

const runtimeEnvironment = computed(() => {
  const target = profile.value?.target
  if (!target)
    return { primary: '—', details: [] as string[] }

  const present = (value: string) => value.trim() !== '' && value !== '—' && value.toLowerCase() !== 'unknown'
  const osType = target.osType === 'macOS'
    ? 'Mac OS'
    : target.osType
  const primary = [osType, target.osVersion].filter(present).join(' ')
  const details = [target.arch, target.terminal].filter(present)
  return {
    primary: primary || '—',
    details,
  }
})
</script>

<template>
  <ZCard as="article" class="flex min-h-95 w-full flex-col">
    <template #header>
      <div class="flex min-w-0 flex-wrap items-start justify-between gap-3">
        <h2 class="m-0 pt-0.5 text-xl leading-[1.15] font-heavy text-z-text text-balance">
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

      <div v-else-if="identity" class="flex flex-1">
        <section
          :aria-label="t('dashboard.clientIdentity.attributes')"
          class="grid min-w-0 flex-1 content-between gap-6 rounded-z-lg bg-z-fill-alter/70 px-5 py-5.5 sm:px-6 sm:py-5"
          :class="
            verifiedLabel || checkedLabel
              ? 'sm:grid-rows-[auto_minmax(0,1fr)_auto_auto]'
              : 'sm:grid-rows-[auto_minmax(0,1fr)_auto]'
          "
        >
          <div class="flex min-w-0 items-center justify-between gap-3">
            <div class="flex min-w-0 items-center gap-2 text-z-text">
              <span class="inline-flex size-7 shrink-0 items-center justify-center rounded-lg bg-z-fill-tertiary">
                <Box class="size-3.75 text-z-text-secondary" />
              </span>
              <span class="truncate text-z-xs leading-none font-heavy" :title="profile.product">{{ profile.product }}</span>
            </div>
            <span
              class="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-lg px-2.5 text-z-sm leading-none font-bold"
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
                class="inline-flex max-w-full min-w-0 flex-wrap items-baseline gap-x-2 gap-y-1 font-mono leading-none tabular-nums"
              >
                <span class="wrap-break-word text-[27px] leading-[1.05] font-heavy text-z-text">
                  {{ versionParts.release }}
                </span>
                <span v-if="versionParts.prerelease" class="text-z-sm font-bold wrap-anywhere text-z-text-secondary">
                  {{ versionParts.prerelease }}
                </span>
              </strong>
              <span
                v-if="profile.build"
                class="shrink-0 font-mono text-[10px] leading-none font-emphasis tabular-nums text-z-text-quaternary"
              >
                {{ t('common.fields.build') }} {{ profile.build }}
              </span>
            </div>
          </div>

          <dl class="m-0 flex min-w-0 flex-wrap gap-x-7 gap-y-5">
            <div v-if="identity.authProtocol" class="min-w-0 flex-auto">
              <dt class="flex items-center gap-1.5 text-[10px] leading-none font-bold text-z-text-quaternary">
                <ShieldCheck class="size-3.25 text-z-text-tertiary" />
                {{ t('dashboard.clientIdentity.authProtocol') }}
              </dt>
              <dd class="mt-2 mb-0 font-mono text-z-lg leading-snug font-bold wrap-anywhere tabular-nums text-z-text">
                {{ identity.authProtocol }}
              </dd>
            </div>

            <div class="min-w-0 flex-auto">
              <dt class="flex items-center gap-1.5 text-[10px] leading-none font-bold text-z-text-quaternary">
                <Monitor class="size-3.25 text-z-text-tertiary" />
                {{ identity.environmentLabel }}
              </dt>
              <dd class="mt-2 mb-0 font-mono text-z-lg leading-snug font-bold wrap-anywhere tabular-nums text-z-text">
                {{ runtimeEnvironment.primary }}
                <span v-if="runtimeEnvironment.details.length" class="text-z-xs font-emphasis text-z-text-secondary">
                  <template v-for="detail in runtimeEnvironment.details" :key="detail"> · {{ detail }} </template>
                </span>
              </dd>
            </div>

            <div v-for="(attribute, index) in identity.attributes" :key="index" class="min-w-0 max-w-full">
              <dt class="flex items-center gap-1.5 text-[10px] leading-none font-bold text-z-text-quaternary">
                <Terminal class="size-3.25 text-z-text-tertiary" />
                {{ attribute.label }}
              </dt>
              <dd class="mt-2 mb-0 font-mono text-[16px] leading-snug font-heavy wrap-anywhere tabular-nums text-z-text">
                {{ attribute.value }}
              </dd>
            </div>
          </dl>

          <footer
            v-if="verifiedLabel || checkedLabel"
            class="flex min-w-0 flex-wrap items-center justify-between gap-x-4 gap-y-1 text-[10px] leading-none font-emphasis text-z-text-quaternary"
          >
            <span v-if="verifiedLabel" :title="profile.userAgent">{{ verifiedLabel }}</span>
            <span v-if="checkedLabel">{{ checkedLabel }}</span>
          </footer>
        </section>
      </div>
    </template>
  </ZCard>
</template>
