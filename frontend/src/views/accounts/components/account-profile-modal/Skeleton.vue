<script setup lang="ts">
import { BaseSkeleton } from '@codex-proxy/ui'
</script>

<template>
  <div class="grid gap-8 sm:gap-10" role="status" aria-live="polite" aria-busy="true">
    <span class="sr-only">正在加载累计活动</span>

    <section class="grid min-w-0 gap-4 overflow-hidden">
      <div class="flex items-center justify-between">
        <BaseSkeleton shape="text" class="h-4 w-20" />
        <BaseSkeleton class="h-7 w-32 rounded-cp opacity-70" />
      </div>
      <div class="grid min-w-190 gap-1" style="grid-template-columns: repeat(52, minmax(0, 1fr))">
        <div v-for="week in 52" :key="week" class="grid gap-1">
          <BaseSkeleton v-for="day in 7" :key="day" class="aspect-square rounded-[3px] opacity-60" />
        </div>
      </div>
    </section>

    <section class="grid gap-10 sm:grid-cols-2">
      <div v-for="column in 2" :key="column" class="grid gap-3">
        <BaseSkeleton shape="text" class="h-4 w-28" />
        <BaseSkeleton v-for="row in 5" :key="row" shape="text" class="w-full opacity-60" />
      </div>
    </section>
  </div>
</template>
