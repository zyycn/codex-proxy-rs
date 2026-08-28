<script setup lang="ts">
import BaseSkeleton from '@/components/base/BaseSkeleton.vue'
</script>

<template>
  <div class="grid gap-8 sm:gap-10" role="status" aria-live="polite" aria-busy="true">
    <span class="sr-only">正在加载官方个人资料</span>

    <section
      class="grid gap-4 rounded-cp-lg bg-cp-bg-elevated p-4 shadow-cp-input sm:p-5 lg:grid-cols-[minmax(310px,1.05fr)_minmax(0,1.95fr)] lg:items-center lg:gap-4"
      aria-hidden="true"
    >
      <div class="flex min-w-0 items-center gap-3.5">
        <BaseSkeleton shape="circle" class="size-14 shrink-0" />
        <div class="grid min-w-0 flex-1 gap-1.5">
          <div class="flex items-center gap-2">
            <BaseSkeleton shape="text" class="h-4 w-20" />
            <BaseSkeleton class="h-5 w-9 rounded-full opacity-70" />
          </div>
          <BaseSkeleton shape="text" class="w-24 opacity-60" />
          <BaseSkeleton shape="text" class="w-full max-w-64 opacity-60" />
        </div>
      </div>

      <div class="min-w-0 overflow-x-auto py-1">
        <div class="grid min-w-130 grid-cols-5">
          <div
            v-for="metric in 5"
            :key="metric"
            class="grid justify-items-center gap-2 px-2 sm:px-3"
          >
            <BaseSkeleton shape="text" class="h-4 w-14" />
            <BaseSkeleton shape="text" class="w-20 opacity-60" />
          </div>
        </div>
      </div>
    </section>

    <section class="grid gap-4" aria-hidden="true">
      <div class="flex items-center justify-between">
        <BaseSkeleton shape="text" class="h-4 w-20" />
        <BaseSkeleton class="h-7 w-32 rounded-cp opacity-70" />
      </div>
      <div
        class="grid min-w-190 gap-1"
        style="grid-template-columns: repeat(52, minmax(0, 1fr));"
      >
        <div v-for="week in 52" :key="week" class="grid gap-1">
          <BaseSkeleton v-for="day in 7" :key="day" class="aspect-square rounded-[3px] opacity-60" />
        </div>
      </div>
    </section>

    <section class="grid gap-10 sm:grid-cols-2" aria-hidden="true">
      <div v-for="column in 2" :key="column" class="grid gap-3">
        <BaseSkeleton shape="text" class="h-4 w-28" />
        <BaseSkeleton v-for="row in 5" :key="row" shape="text" class="w-full opacity-60" />
      </div>
    </section>
  </div>
</template>
