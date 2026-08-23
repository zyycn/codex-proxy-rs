<script setup lang="ts">
import { Openai, Xai } from '@boxicons/vue'
import { Box, CheckCircle2, Monitor, RefreshCw, ShieldCheck, Terminal, TriangleAlert } from '@lucide/vue'

import { computed, shallowRef, watch } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import { formatDateTime } from '@/utils/date'
import { formatProviderLabel } from '@/utils/providers'

interface WireProfile {
  provider: string
  product: string
  version: string
  build?: string | null
  target: {
    osType: string
    osVersion: string
    arch: string
    terminal: string
  }
  userAgent: string
  attributes: Array<{ label: string, value: string }>
  verifiedAt?: string | null
  release?: {
    status?: 'unchecked' | 'aligned' | 'review_required' | 'check_failed'
    checkedAt?: string | null
    latestVersion?: string | null
    latestBuild?: string | null
    error?: string | null
  } | null
}

const props = defineProps<{
  profiles: WireProfile[]
}>()

const activeProvider = shallowRef('')

const providerOptions = computed(() =>
  props.profiles.map(profile => ({
    label: providerLabel(profile.provider),
    value: profile.provider,
    icon: profile.provider === 'openai' ? Openai : profile.provider === 'xai' ? Xai : undefined,
  })),
)

const profile = computed(() =>
  props.profiles.find(item => item.provider === activeProvider.value) ?? props.profiles[0] ?? null,
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
    return '尚未检查'
  return release.latestBuild
    ? `${release.latestVersion} · Build ${release.latestBuild}`
    : release.latestVersion
})

const releaseStatus = computed(() => {
  const current = profile.value
  if (!current?.release) {
    return {
      label: '当前生效',
      title: '当前 Provider 请求正在使用此运行时画像',
      tone: 'bg-cp-info-bg text-cp-info-text',
      icon: ShieldCheck,
    }
  }

  const status = current.release.status ?? 'unchecked'
  if (status === 'aligned') {
    return {
      label: '制品一致',
      title: '当前生效版本与官方最新发布一致',
      tone: 'bg-cp-success-bg text-cp-success-text',
      icon: CheckCircle2,
    }
  }
  if (status === 'review_required') {
    return {
      label: '发现新版',
      title: `官方最新版本 ${releaseLabel.value}`,
      tone: 'bg-cp-warning-bg text-cp-warning-text',
      icon: TriangleAlert,
    }
  }
  if (status === 'check_failed') {
    return {
      label: '检查失败',
      title: current.release.error || '官方版本检查失败',
      tone: 'bg-cp-error-bg text-cp-error-text',
      icon: TriangleAlert,
    }
  }
  return {
    label: '待检查',
    title: '尚未检查官方发布渠道',
    tone: 'bg-cp-status-normal-bg text-cp-status-normal-text',
    icon: RefreshCw,
  }
})

const verifiedLabel = computed(() =>
  profile.value?.verifiedAt ? `画像核验 ${formatDateTime(profile.value.verifiedAt)}` : undefined,
)

const checkedLabel = computed(() => {
  const checkedAt = profile.value?.release?.checkedAt
  return checkedAt ? `发布检查 ${formatDateTime(checkedAt)}` : undefined
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
  const value = current.attributes.find(attribute => attribute.label === '客户端标识')?.value ?? '—'
  return current.provider === 'xai' ? toPascalCase(value) : value
})

const authProtocol = computed(() => {
  const value = profile.value?.attributes.find(attribute => attribute.label === 'Token 认证')?.value
  if (!value)
    return '—'
  return toPascalCase(value)
})

const runtimeEnvironment = computed(() => {
  const target = profile.value?.target
  if (!target)
    return { primary: '—', details: [] as string[], title: '—' }

  const present = (value: string) => value !== '—' && value.toLowerCase() !== 'unknown'
  const primary = [target.osType, target.osVersion]
    .filter(present)
    .join(' ')
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

function providerLabel(provider: string) {
  return formatProviderLabel(provider)
}
</script>

<template>
  <BaseCard
    as="article"
    title="上游请求身份"
    class="flex min-h-95 w-full flex-col"
  >
    <template #actions>
      <BaseSegmented
        v-if="providerOptions.length > 1"
        v-model="activeProvider"
        label="上游平台"
        :options="providerOptions"
        display="icon"
        class="w-21"
      />
    </template>

    <template #body>
      <BaseEmpty
        v-if="!profile"
        title="暂无请求身份"
        class="min-h-71.75 flex-1 place-content-center"
      />

      <div v-else class="flex flex-1">
        <section
          aria-label="请求身份组成"
          class="grid min-w-0 flex-1 content-between gap-6 rounded-cp-lg bg-cp-bg-elevated px-5 py-5.5 sm:px-6 sm:py-5"
          :class="verifiedLabel || checkedLabel
            ? 'sm:grid-rows-[auto_minmax(0,1fr)_auto_auto]'
            : 'sm:grid-rows-[auto_minmax(0,1fr)_auto]'"
        >
          <div class="flex min-w-0 items-center justify-between gap-3">
            <div class="flex min-w-0 items-center gap-2 text-cp-text">
              <span
                class="inline-flex size-7 shrink-0 items-center justify-center rounded-lg bg-cp-fill-tertiary"
              >
                <Box aria-hidden="true" class="size-3.75 text-cp-text-secondary" />
              </span>
              <span class="truncate text-cp-xs leading-none font-heavy">{{ profile.product }}</span>
            </div>
            <span
              class="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-lg px-2.5 text-cp-sm leading-none font-bold"
              :class="releaseStatus.tone"
              :title="releaseStatus.title"
            >
              <component :is="releaseStatus.icon" aria-hidden="true" class="size-3.5" />
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
                <span
                  v-if="versionParts.prerelease"
                  aria-hidden="true"
                  class="truncate text-cp-sm font-bold text-cp-text-secondary"
                >
                  {{ versionParts.prerelease }}
                </span>
              </strong>
              <span
                v-if="profile.build"
                class="shrink-0 font-mono text-[10px] leading-none font-emphasis tabular-nums text-cp-text-quaternary"
              >
                Build {{ profile.build }}
              </span>
            </div>
          </div>

          <dl
            class="m-0 grid min-w-0 gap-5 sm:gap-7"
            :class="profile.provider === 'xai'
              ? 'sm:grid-cols-[0.62fr_1.28fr_0.94fr]'
              : 'sm:grid-cols-[0.72fr_1.28fr]'"
          >
            <div v-if="profile.provider === 'xai'" class="min-w-0">
              <dt
                class="flex items-center gap-1.5 text-[10px] leading-none font-bold text-cp-text-quaternary"
              >
                <ShieldCheck aria-hidden="true" class="size-3.25 text-cp-info" />
                认证协议
              </dt>
              <dd
                class="mt-2 mb-0 truncate font-mono text-cp-lg leading-none font-bold tabular-nums text-cp-text"
                :title="authProtocol"
              >
                {{ authProtocol }}
              </dd>
            </div>

            <div class="min-w-0">
              <dt
                class="flex items-center gap-1.5 text-[10px] leading-none font-bold text-cp-text-quaternary"
              >
                <Monitor aria-hidden="true" class="size-3.25 text-cp-status-normal" />
                {{ profile.provider === 'openai' ? '模拟运行环境' : '运行环境' }}
              </dt>
              <dd
                class="mt-2 mb-0 truncate font-mono text-cp-lg leading-none font-bold tabular-nums text-cp-text"
                :title="runtimeEnvironment.title"
              >
                {{ runtimeEnvironment.primary }}
                <span
                  v-if="runtimeEnvironment.details.length"
                  class="text-cp-xs font-emphasis text-cp-text-secondary"
                >
                  <template v-for="detail in runtimeEnvironment.details" :key="detail">
                    · {{ detail }}
                  </template>
                </span>
              </dd>
            </div>

            <div class="min-w-0">
              <dt
                class="flex items-center gap-1.5 text-[10px] leading-none font-bold text-cp-text-quaternary"
              >
                <Terminal aria-hidden="true" class="size-3.25 text-cp-info" />
                客户端标识
              </dt>
              <dd
                class="mt-2 mb-0 truncate font-mono text-[16px] leading-none font-heavy tabular-nums text-cp-text"
                :title="clientIdentity"
              >
                {{ clientIdentity }}
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
  </BaseCard>
</template>
