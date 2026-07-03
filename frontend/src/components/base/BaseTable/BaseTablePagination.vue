<script setup lang="ts">
import { ChevronLeft, ChevronRight } from '@lucide/vue'
import { computed } from 'vue'

import BaseSelect from '../BaseSelect.vue'
import {
  getCurrentPage,
  getPageSizeOptions,
  getPagerItems,
  getTotalPages,
  type BaseTablePagination,
} from './pagination'

const props = defineProps<{
  pagination: BaseTablePagination
  loading: boolean
}>()

const emit = defineEmits<{
  'page-change': [page: number]
  'page-size-change': [pageSize: number]
}>()

const totalPages = computed(() => getTotalPages(props.pagination))
const currentPage = computed(() => getCurrentPage(props.pagination, totalPages.value))
const pageSizeOptions = computed(() => getPageSizeOptions(props.pagination))
const pagerItems = computed(() => getPagerItems(totalPages.value, currentPage.value))

const pageSizeModel = computed({
  get: () => String(props.pagination.pageSize),
  set: (value: string) => {
    if (props.loading) {
      return
    }

    const pageSize = Number(value)
    if (Number.isFinite(pageSize) && pageSize > 0) {
      emit('page-size-change', pageSize)
    }
  },
})

function goToPage(page: number) {
  if (props.loading || page < 1 || page > totalPages.value || page === currentPage.value) {
    return
  }

  emit('page-change', page)
}

function paginationButtonClass(disabled: boolean) {
  return [
    'inline-flex size-8 items-center justify-center rounded-(--cp-input-radius-base) border-0 bg-(--cp-bg-subtle) text-(--cp-text-secondary) transition-colors duration-150 outline-none',
    disabled
      ? 'cursor-not-allowed opacity-45 shadow-none'
      : 'cursor-pointer hover:bg-(--cp-default-bg-hover) hover:text-(--cp-text-primary) focus-visible:ring-2 focus-visible:ring-(--cp-info-border) focus-visible:ring-offset-2 focus-visible:ring-offset-(--cp-bg-surface)',
  ]
}

function paginationPageClass(page: number) {
  return [
    'inline-flex size-8 items-center justify-center rounded-(--cp-input-radius-base) border-0 text-xs font-[720] leading-none transition-colors duration-150 outline-none',
    page === currentPage.value
      ? 'cursor-default bg-(--cp-info) text-(--cp-info-on)'
      : 'cursor-pointer bg-(--cp-bg-subtle) text-(--cp-text-primary) hover:bg-(--cp-default-bg-hover) focus-visible:ring-2 focus-visible:ring-(--cp-info-border) focus-visible:ring-offset-2 focus-visible:ring-offset-(--cp-bg-surface)',
  ]
}
</script>

<template>
  <footer
    class="mt-2 flex min-h-10 shrink-0 flex-wrap items-center justify-between gap-3 px-0 py-1"
  >
    <div
      class="flex min-w-0 items-center gap-2.5 text-[12px] font-[650] text-(--cp-text-secondary)"
    >
      <span class="whitespace-nowrap">共 {{ pagination.total }} 条</span>
    </div>

    <div class="flex items-center gap-2">
      <BaseSelect
        v-model="pageSizeModel"
        :options="pageSizeOptions"
        :disabled="loading"
        size="compact"
        class="w-28"
      />

      <div class="flex items-center gap-2">
        <button
          type="button"
          :class="paginationButtonClass(loading || currentPage <= 1)"
          :disabled="loading || currentPage <= 1"
          title="上一页"
          aria-label="上一页"
          @click="goToPage(currentPage - 1)"
        >
          <ChevronLeft class="size-4" />
        </button>

        <template v-for="(item, index) in pagerItems" :key="`${item}-${index}`">
          <span
            v-if="item === 'ellipsis'"
            class="inline-flex size-8 items-center justify-center text-xs font-[720] text-(--cp-text-muted)"
          >
            ...
          </span>
          <button
            v-else
            type="button"
            :class="paginationPageClass(item)"
            :disabled="loading || item === currentPage"
            :aria-current="item === currentPage ? 'page' : undefined"
            @click="goToPage(item)"
          >
            {{ item }}
          </button>
        </template>

        <button
          type="button"
          :class="paginationButtonClass(loading || currentPage >= totalPages)"
          :disabled="loading || currentPage >= totalPages"
          title="下一页"
          aria-label="下一页"
          @click="goToPage(currentPage + 1)"
        >
          <ChevronRight class="size-4" />
        </button>
      </div>
    </div>
  </footer>
</template>
