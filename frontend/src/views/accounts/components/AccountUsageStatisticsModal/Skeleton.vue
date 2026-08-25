<script setup lang="ts">
import BaseSkeleton from '@/components/base/BaseSkeleton.vue'
</script>

<template>
  <div
    class="grid gap-5"
    role="status"
    aria-live="polite"
    aria-busy="true"
  >
    <span class="sr-only">正在加载官方用量统计</span>

    <section
      class="grid gap-2 rounded-cp-lg bg-cp-bg-elevated px-3 py-2 sm:grid-cols-[1fr_auto_1fr] sm:items-center"
      aria-hidden="true"
    >
      <div class="flex items-center gap-1">
        <BaseSkeleton shape="circle" class="size-8 opacity-70" />
        <BaseSkeleton shape="circle" class="size-8 opacity-70" />
      </div>
      <div class="flex min-w-0 flex-wrap items-center gap-1.5 sm:justify-center">
        <BaseSkeleton shape="text" class="w-36" />
        <BaseSkeleton class="h-5 w-12 rounded-full opacity-70" />
        <BaseSkeleton class="h-5 w-14 rounded-full opacity-70" />
      </div>
      <BaseSkeleton shape="text" class="w-36 opacity-60 sm:justify-self-end" />
    </section>

    <section class="overflow-hidden rounded-cp-lg bg-cp-bg-elevated" aria-hidden="true">
      <div class="grid gap-5 p-4 sm:grid-cols-[minmax(0,1.35fr)_minmax(0,1fr)] sm:p-5">
        <div class="min-w-0">
          <BaseSkeleton shape="text" class="w-20 opacity-60" />
          <BaseSkeleton class="mt-2 h-7 w-24" />
          <BaseSkeleton class="mt-4 h-1.5 w-full rounded-full opacity-70" />
        </div>

        <div class="grid grid-cols-2 gap-x-4 gap-y-3 border-t border-cp-split pt-4 sm:border-t-0 sm:border-l sm:pt-0 sm:pl-5">
          <div v-for="metric in 4" :key="metric" class="grid gap-2">
            <BaseSkeleton shape="text" class="w-16 opacity-60" />
            <BaseSkeleton shape="text" class="h-3.5 w-14" />
          </div>
        </div>
      </div>

      <div class="grid grid-cols-2 border-t border-cp-split sm:grid-cols-4">
        <div
          v-for="metric in 4"
          :key="metric"
          class="grid gap-2 px-4 py-3 sm:border-l sm:border-cp-split sm:px-5 sm:first:border-l-0"
        >
          <BaseSkeleton shape="text" class="w-16 opacity-60" />
          <BaseSkeleton shape="text" class="h-3.5 w-12" />
        </div>
      </div>
    </section>

    <section
      v-for="section in 2"
      :key="section"
      class="min-w-0"
      aria-hidden="true"
    >
      <div class="mb-2.5 flex items-center justify-between gap-3">
        <BaseSkeleton shape="text" class="w-20" />
        <BaseSkeleton shape="text" class="w-24 opacity-50" />
      </div>

      <div class="overflow-hidden rounded-cp">
        <div class="grid grid-cols-[1.4fr_repeat(6,minmax(0,1fr))] items-center gap-3 bg-cp-fill-quaternary px-3 py-2">
          <BaseSkeleton
            v-for="column in 7"
            :key="column"
            shape="text"
            class="h-2.5 opacity-60"
            :class="column === 1 ? 'w-3/4' : 'ml-auto w-2/3'"
          />
        </div>
        <div
          v-for="row in 2"
          :key="row"
          class="grid grid-cols-[1.4fr_repeat(6,minmax(0,1fr))] items-center gap-3 px-3 py-2.5"
          :class="row % 2 === 0 ? 'bg-cp-fill-quaternary/55' : undefined"
        >
          <BaseSkeleton
            v-for="column in 7"
            :key="column"
            shape="text"
            class="opacity-70"
            :class="column === 1 ? 'w-4/5' : 'ml-auto w-3/5'"
          />
        </div>
      </div>
    </section>
  </div>
</template>
