<script setup lang="ts">
import { Binary, Box, CheckCircle2, Monitor, RefreshCw, TriangleAlert } from '@lucide/vue'
import { computed } from 'vue'

import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import { formatDateTime } from '@/utils/date'

const props = defineProps<{
  profile: any
}>()

const releaseLabel = computed(() => {
  const release = props.profile?.release
  if (!release?.latestVersion) return '尚未检查'
  return release.latestBuild
    ? `${release.latestVersion} · Build ${release.latestBuild}`
    : release.latestVersion
})

const releaseStatus = computed(() => {
  const status = props.profile?.release?.status ?? 'unchecked'
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
      title: props.profile?.release?.error || 'Desktop 发布检查失败',
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

const verifiedAt = computed(() =>
  props.profile?.verifiedAt ? formatDateTime(props.profile.verifiedAt) : '—',
)

const checkedAt = computed(() =>
  props.profile?.release?.checkedAt ? formatDateTime(props.profile.release.checkedAt) : '—',
)
</script>

<template>
  <BaseCard as="article" variant="dashboard" title="上游请求身份" class="min-h-95 w-full">
    <template #actions>
      <span
        class="inline-flex h-7 items-center gap-1.5 rounded-lg px-2.5 text-[12px] leading-none font-[720]"
        :class="releaseStatus.tone"
        :title="releaseStatus.title"
      >
        <component :is="releaseStatus.icon" class="size-3.5" />
        {{ releaseStatus.label }}
      </span>
    </template>

    <template #body>
      <BaseEmpty v-if="!profile" compact title="暂无请求身份" class="h-67 place-content-center" />

      <div
        v-else
        class="mt-5 grid gap-3 max-[767px]:pl-5 sm:h-[287px] sm:grid-rows-[minmax(0,1fr)_24px]"
      >
        <section
          aria-label="请求身份组成"
          class="grid min-w-0 content-between gap-6 rounded-[14px] bg-(--cp-bg-subtle) px-5 py-5.5 sm:h-full sm:grid-rows-[auto_minmax(0,1fr)_auto] sm:px-6 sm:py-5"
        >
          <div class="flex min-w-0 items-center justify-between gap-4">
            <div class="flex min-w-0 items-center gap-2 text-(--cp-success-text)">
              <span
                class="inline-flex size-7 shrink-0 items-center justify-center rounded-lg bg-(--cp-success-bg)"
              >
                <Box class="size-3.75 text-(--cp-success)" />
              </span>
              <span class="truncate text-[11px] leading-none font-[760]">Codex Desktop</span>
            </div>
            <span
              class="shrink-0 font-mono text-[10px] leading-none font-[650] tabular-nums text-(--cp-text-muted)"
            >
              Build {{ profile.desktopBuild }}
            </span>
          </div>

          <div class="grid min-h-0 content-center">
            <strong
              class="block max-w-full wrap-break-word font-mono text-[27px] leading-[1.05] font-[790] tabular-nums text-(--cp-text-primary)"
              :title="profile.desktopVersion"
            >
              {{ profile.desktopVersion }}
            </strong>
          </div>

          <dl class="m-0 grid min-w-0 gap-5 sm:grid-cols-[0.72fr_1.28fr] sm:gap-7">
            <div class="min-w-0">
              <dt
                class="flex items-center gap-1.5 text-[10px] leading-none font-[720] text-(--cp-text-muted)"
              >
                <Binary class="size-3.25 text-(--cp-info)" />
                Codex Core
              </dt>
              <dd
                class="mt-2 mb-0 truncate font-mono text-[16px] leading-none font-[760] tabular-nums text-(--cp-text-primary)"
                :title="profile.codexVersion"
              >
                {{ profile.codexVersion }}
              </dd>
            </div>

            <div class="min-w-0">
              <dt
                class="flex items-center gap-1.5 text-[10px] leading-none font-[720] text-(--cp-text-muted)"
              >
                <Monitor class="size-3.25 text-(--cp-normal)" />
                模拟运行环境
              </dt>
              <dd
                class="mt-2 mb-0 truncate font-mono text-[14px] leading-none font-[720] tabular-nums text-(--cp-text-primary)"
                :title="`${profile.target.osType} ${profile.target.osVersion} · ${profile.target.arch}`"
              >
                {{ profile.target.osType }} {{ profile.target.osVersion }}
                <span class="text-[11px] font-[650] text-(--cp-text-secondary)">
                  · {{ profile.target.arch }}
                </span>
              </dd>
            </div>
          </dl>
        </section>

        <footer
          class="flex min-w-0 flex-wrap items-center justify-between gap-x-4 gap-y-1 text-[10px] leading-none font-[650] text-(--cp-text-muted)"
        >
          <time :datetime="profile.verifiedAt">源码审计 {{ verifiedAt }}</time>
          <time :datetime="profile.release?.checkedAt">发布检查 {{ checkedAt }}</time>
        </footer>
      </div>
    </template>
  </BaseCard>
</template>
