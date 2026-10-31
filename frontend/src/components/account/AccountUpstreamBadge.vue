<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import AppBrandMark from '@/components/AppBrandMark.vue'

withDefaults(defineProps<{ size?: 'xs' | 'sm' | 'md' }>(), { size: 'md' })

const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <span
    class="peer-badge relative isolate inline-flex max-w-full shrink-0 cursor-default items-center gap-0.5 overflow-hidden rounded-md bg-cp-cyan-container-strong py-0.5 pr-1.5 pl-0.5 align-middle text-cp-cyan-on-container"
    :class="size === 'xs' ? 'h-4.5' : 'h-5'"
    :aria-label="t('accounts.upstream.link')"
    :title="t('accounts.upstream.link')"
  >
    <AppBrandMark variant="glyph" class="relative size-4 shrink-0" aria-hidden="true" />
    <span class="relative font-mono text-[10px] leading-none font-heavy tracking-[0.05em]">CPR</span>
  </span>
</template>

<style scoped>
.peer-badge::after {
  position: absolute;
  inset: 0;
  z-index: -1;
  pointer-events: none;
  content: '';
  background: linear-gradient(
    110deg,
    transparent 30%,
    color-mix(in srgb, var(--cp-color-text-light-solid) 12%, transparent) 43%,
    color-mix(in srgb, var(--cp-color-text-light-solid) 36%, transparent) 50%,
    transparent 70%
  );
  transform: translateX(-120%);
}

@media (hover: hover) and (prefers-reduced-motion: no-preference) {
  .peer-badge:hover::after {
    animation: peer-glint 650ms ease-out;
  }
}

@keyframes peer-glint {
  to {
    transform: translateX(120%);
  }
}
</style>
