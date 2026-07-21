<script setup lang="ts">
import { Openai, Xai } from '@boxicons/vue'
import { Box, CheckCircle2, Monitor, RefreshCw, ShieldCheck, Terminal, TriangleAlert } from '@lucide/vue'

import { computed, shallowRef, watch } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import { formatDateTime } from '@/utils/date'

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
      label: '官方固定画像',
      title: '当前 Provider 使用官方客户端固定画像',
      tone: 'bg-(--cp-info-bg) text-(--cp-info-text)',
      icon: ShieldCheck,
    }
  }

  const status = current.release.status ?? 'unchecked'
  if (status === 'aligned') {
    return {
      label: '制品一致',
      title: '当前 Desktop 版本与官方发布一致',
      tone: 'bg-(--cp-success-bg) text-(--cp-success-text)',
      icon: CheckCircle2,
    }
  }
  if (status === 'review_required') {
    return {
      label: '发现新版',
      title: `官方最新版本 ${releaseLabel.value}`,
      tone: 'bg-(--cp-warning-bg) text-(--cp-warning-text)',
      icon: TriangleAlert,
    }
  }
  if (status === 'check_failed') {
    return {
      label: '检查失败',
      title: current.release.error || 'Desktop 发布检查失败',
      tone: 'bg-(--cp-danger-bg) text-(--cp-danger-text)',
      icon: TriangleAlert,
    }
  }
  return {
    label: '待检查',
    title: '尚未检查 Desktop 官方发布',
    tone: 'bg-(--cp-normal-bg) text-(--cp-normal-text)',
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
  const label = current.provider === 'openai' ? 'Codex Core' : '客户端标识'
  const value = current.attributes.find(attribute => attribute.label === label)?.value ?? '—'
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
    .map(value => value.toLowerCase() === 'linux' ? 'Linux' : value)
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
  if (provider === 'openai')
    return 'OpenAI'
  if (provider === 'xai')
    return 'xAI'
  return provider
}
</script>

<template>
  <BaseCard
    as="article"
    variant="dashboard"
    title="上游请求身份"
    body-class="flex min-h-0 flex-1 flex-col"
    class="flex min-h-95 w-full flex-col"
  >
    <template #actions>
      <BaseSegmented
        v-if="providerOptions.length > 1"
        v-model="activeProvider"
        :options="providerOptions"
        icon-only
        class="w-21"
      />
    </template>

    <template #body>
      <BaseEmpty
        v-if="!profile"
        compact
        title="暂无请求身份"
        class="mt-5 min-h-71.75 flex-1 place-content-center"
      />

      <div v-else class="mt-5 flex flex-1">
        <section
          aria-label="请求身份组成"
          class="grid min-w-0 flex-1 content-between gap-6 rounded-[14px] bg-(--cp-bg-subtle) px-5 py-5.5 sm:px-6 sm:py-5"
          :class="verifiedLabel || checkedLabel || profile.provider === 'xai'
            ? 'sm:grid-rows-[auto_minmax(0,1fr)_auto_auto]'
            : 'sm:grid-rows-[auto_minmax(0,1fr)_auto]'"
        >
          <div class="flex min-w-0 items-center justify-between gap-3">
            <div class="flex min-w-0 items-center gap-2 text-(--cp-text-primary)">
              <span
                class="inline-flex size-7 shrink-0 items-center justify-center rounded-lg bg-(--cp-bg-muted)"
              >
                <Box aria-hidden="true" class="size-3.75 text-(--cp-text-secondary)" />
              </span>
              <span class="truncate text-[11px] leading-none font-[760]">{{ profile.product }}</span>
            </div>
            <span
              class="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-lg px-2.5 text-[12px] leading-none font-[720]"
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
                class="block max-w-full wrap-break-word font-mono text-[27px] leading-[1.05] font-[790] tabular-nums text-(--cp-text-primary)"
                :title="profile.version"
              >
                {{ profile.version }}
              </strong>
              <span
                v-if="profile.build"
                class="shrink-0 font-mono text-[10px] leading-none font-[650] tabular-nums text-(--cp-text-muted)"
              >
                Build {{ profile.build }}
              </span>
            </div>
          </div>

          <dl v-if="profile.provider === 'xai'" class="m-0 min-w-0">
            <div class="min-w-0">
              <dt
                class="flex items-center gap-1.5 text-[10px] leading-none font-[720] text-(--cp-text-muted)"
              >
                <ShieldCheck aria-hidden="true" class="size-3.25 text-(--cp-info)" />
                认证协议
              </dt>
              <dd
                class="mt-2 mb-0 truncate font-mono text-[14px] leading-none font-[720] tabular-nums text-(--cp-text-primary)"
                :title="authProtocol"
              >
                {{ authProtocol }}
              </dd>
            </div>
          </dl>

          <dl class="m-0 grid min-w-0 gap-5 sm:grid-cols-[0.72fr_1.28fr] sm:gap-7">
            <div class="min-w-0">
              <dt
                class="flex items-center gap-1.5 text-[10px] leading-none font-[720] text-(--cp-text-muted)"
              >
                <Monitor aria-hidden="true" class="size-3.25 text-(--cp-normal)" />
                {{ profile.provider === 'openai' ? '模拟运行环境' : '运行环境' }}
              </dt>
              <dd
                class="mt-2 mb-0 truncate font-mono text-[14px] leading-none font-[720] tabular-nums text-(--cp-text-primary)"
                :title="runtimeEnvironment.title"
              >
                {{ runtimeEnvironment.primary }}
                <span
                  v-if="runtimeEnvironment.details.length"
                  class="text-[11px] font-[650] text-(--cp-text-secondary)"
                >
                  <template v-for="detail in runtimeEnvironment.details" :key="detail">
                    · {{ detail }}
                  </template>
                </span>
              </dd>
            </div>

            <div class="min-w-0">
              <dt
                class="flex items-center gap-1.5 text-[10px] leading-none font-[720] text-(--cp-text-muted)"
              >
                <Terminal aria-hidden="true" class="size-3.25 text-(--cp-info)" />
                {{ profile.provider === 'openai' ? 'Codex Core' : 'Grok Client' }}
              </dt>
              <dd
                class="mt-2 mb-0 truncate font-mono text-[16px] leading-none font-[760] tabular-nums text-(--cp-text-primary)"
                :title="clientIdentity"
              >
                {{ clientIdentity }}
              </dd>
            </div>
          </dl>

          <footer
            v-if="verifiedLabel || checkedLabel"
            class="flex min-w-0 flex-wrap items-center justify-between gap-x-4 gap-y-1 text-[10px] leading-none font-[650] text-(--cp-text-muted)"
          >
            <span v-if="verifiedLabel" :title="profile.userAgent">{{ verifiedLabel }}</span>
            <span v-if="checkedLabel">{{ checkedLabel }}</span>
          </footer>
        </section>
      </div>
    </template>
  </BaseCard>
</template>
