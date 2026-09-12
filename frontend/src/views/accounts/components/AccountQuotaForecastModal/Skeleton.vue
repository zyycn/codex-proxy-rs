<script setup lang="ts">
import BaseSkeleton from '@/components/base/BaseSkeleton.vue'
</script>

<template>
  <div role="status" aria-busy="true">
    <span class="sr-only">正在加载额度预测</span>
    <div class="grid items-start gap-4 md:grid-cols-[1.15fr_1fr] md:items-stretch" aria-hidden="true">
      <div class="flex min-h-96 flex-col justify-between gap-5 rounded-cp-card bg-cp-fill-tertiary/70 p-4 [html[data-theme=light]_&]:bg-cp-fill-quaternary/70">
        <div>
          <div class="flex items-center justify-between gap-3">
            <BaseSkeleton shape="text" class="w-16" />
            <BaseSkeleton class="h-6 w-28" />
          </div>
          <BaseSkeleton class="mt-2 h-8 w-28" />
          <BaseSkeleton class="mt-3 h-3 w-full rounded-full" />
          <div class="mt-2 flex justify-between">
            <BaseSkeleton v-for="tick in 5" :key="tick" shape="text" class="h-2.5 w-5" />
          </div>
        </div>

        <div class="grid gap-2">
          <div class="mb-1 flex justify-between gap-3 px-3">
            <BaseSkeleton shape="text" class="w-20" />
            <BaseSkeleton shape="text" class="w-20" />
          </div>
          <div v-for="metric in 2" :key="metric" class="rounded-cp bg-cp-bg-container px-3 py-3">
            <BaseSkeleton shape="text" class="w-20" />
            <div class="mt-3 grid grid-cols-[1fr_20px_1.2fr] items-center gap-2">
              <BaseSkeleton shape="text" class="w-14" />
              <BaseSkeleton class="size-3" />
              <BaseSkeleton class="h-6 w-24 justify-self-end" />
            </div>
          </div>
        </div>

        <div class="flex justify-between gap-3">
          <BaseSkeleton shape="text" class="w-28" />
          <BaseSkeleton shape="text" class="w-32" />
        </div>
      </div>

      <div class="flex min-h-96 flex-col justify-between gap-5 rounded-cp-card bg-cp-fill-tertiary/70 p-4 [html[data-theme=light]_&]:bg-cp-fill-quaternary/70">
        <div class="grid gap-3">
          <div class="grid gap-1.5">
            <div v-for="row in 2" :key="row" class="flex justify-between gap-3">
              <BaseSkeleton shape="text" class="w-20" />
              <BaseSkeleton shape="text" :class="row === 1 ? 'w-8' : 'w-24'" />
            </div>
          </div>
          <div class="grid gap-2">
            <div v-for="row in 3" :key="row" class="flex h-9.5 items-center justify-between gap-3 rounded-cp bg-cp-bg-container px-3">
              <div class="flex items-center gap-2">
                <BaseSkeleton shape="circle" class="size-1.5" />
                <BaseSkeleton shape="text" class="w-16" />
              </div>
              <BaseSkeleton shape="text" class="w-12" />
            </div>
          </div>
        </div>

        <div class="grid gap-2">
          <div class="flex justify-between gap-3">
            <BaseSkeleton shape="text" class="w-24" />
            <BaseSkeleton shape="text" class="w-12" />
          </div>
          <BaseSkeleton class="h-1 w-full rounded-full" />
          <BaseSkeleton shape="text" class="w-3/4" />
        </div>

        <div class="grid gap-3">
          <div v-for="row in 4" :key="row" class="flex justify-between gap-3">
            <BaseSkeleton shape="text" class="w-20" />
            <BaseSkeleton shape="text" class="w-40 max-w-3/5" />
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
