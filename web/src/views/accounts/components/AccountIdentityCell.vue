<script setup lang="ts">
import { computed } from 'vue'

const props = withDefaults(
  defineProps<{
    account: any
    size?: 'md' | 'lg'
  }>(),
  {
    size: 'md',
  },
)

const displayTitle = computed(
  () =>
    props.account.label?.trim() ||
    props.account.email ||
    props.account.accountId ||
    props.account.id,
)

const secondaryText = computed(() => {
  const title = displayTitle.value
  const secondary = [
    props.account.email,
    props.account.accountId,
    props.account.userId,
    props.account.id,
  ].find((value) => value && value !== title)
  return secondary || props.account.id
})

const initial = computed(() => displayTitle.value.slice(0, 1).toUpperCase())

const avatarSizeClass = computed(() =>
  props.size === 'lg' ? 'size-10 text-[15px]' : 'size-9 text-[13px]',
)

const secondaryClass = computed(() =>
  props.size === 'lg'
    ? 'mt-1 text-[12px] text-(--cp-text-secondary)'
    : 'mt-0.5 font-mono text-[11px] text-(--cp-text-muted)',
)

const avatarClass = computed(() => {
  const palettes = [
    'bg-(--cp-info-bg) text-(--cp-info-text) shadow-[inset_0_0_0_1px_var(--cp-info-border)]',
    'bg-(--cp-success-bg) text-(--cp-success-text) shadow-[inset_0_0_0_1px_var(--cp-success-border)]',
    'bg-(--cp-normal-bg) text-(--cp-normal-text) shadow-[inset_0_0_0_1px_var(--cp-normal-border)]',
    'bg-(--cp-warning-bg) text-(--cp-warning-text) shadow-[inset_0_0_0_1px_var(--cp-warning-border)]',
  ]
  const key = String(props.account.id || props.account.email || displayTitle.value)
  const hash = [...key].reduce((sum, char) => sum + char.charCodeAt(0), 0)
  return palettes[hash % palettes.length]
})
</script>

<template>
  <div class="flex min-w-0 items-center gap-3">
    <span
      class="inline-flex shrink-0 items-center justify-center rounded-lg font-[820]"
      :class="[avatarSizeClass, avatarClass]"
    >
      {{ initial }}
    </span>
    <div class="min-w-0">
      <div class="truncate text-[14px] font-[760] text-(--cp-text-primary)">
        {{ displayTitle }}
      </div>
      <div class="truncate font-[650]" :class="secondaryClass">
        {{ secondaryText }}
      </div>
    </div>
  </div>
</template>
