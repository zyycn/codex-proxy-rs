<script setup lang="ts">
import type { BaseTablePagination } from './pagination'
import { ChevronLeft, ChevronRight } from '@lucide/vue'

import { computed } from 'vue'
import BaseIconButton from '../BaseIconButton.vue'
import BaseSelect from '../BaseSelect.vue'
import {
  getCurrentPage,
  getPagerItems,
  getPageSizeOptions,
  getTotalPages,
} from './pagination'

const props = defineProps<{
  pagination: BaseTablePagination
  loading: boolean
}>()

const emit = defineEmits<{
  pageChange: [page: number]
  pageSizeChange: [pageSize: number]
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
      emit('pageSizeChange', pageSize)
    }
  },
})

function goToPage(page: number) {
  if (props.loading || page < 1 || page > totalPages.value || page === currentPage.value) {
    return
  }

  emit('pageChange', page)
}

function paginationPageClass(page: number) {
  return [
    'inline-flex size-8 items-center justify-center rounded-cp-control border-0 text-xs font-bold leading-none transition-colors duration-150 outline-none',
    page === currentPage.value
      ? 'cursor-default bg-cp-accent text-cp-accent-on'
      : 'cursor-pointer bg-cp-subtle text-cp-primary hover:bg-cp-default-hover focus-visible:ring-2 focus-visible:ring-cp-accent-border focus-visible:ring-offset-2 focus-visible:ring-offset-cp-surface',
  ]
}
</script>

<template>
  <footer
    class="mt-2 flex min-h-10 shrink-0 flex-wrap items-center justify-between gap-3 px-0 py-1"
  >
    <div
      class="flex min-w-0 items-center gap-2.5 text-[12px] font-emphasis text-cp-secondary"
    >
      <span class="whitespace-nowrap">共 {{ pagination.total }} 条</span>
    </div>

    <div class="flex items-center gap-2">
      <BaseSelect
        v-model="pageSizeModel"
        aria-label="每页条数"
        :options="pageSizeOptions"
        :disabled="loading"
        size="sm"
        class="w-28"
      />

      <div class="flex items-center gap-2">
        <BaseIconButton
          variant="secondary"
          size="sm"
          :disabled="loading || currentPage <= 1"
          label="上一页"
          @click="goToPage(currentPage - 1)"
        >
          <ChevronLeft class="size-4" />
        </BaseIconButton>

        <template v-for="(item, index) in pagerItems" :key="`${item}-${index}`">
          <span
            v-if="item === 'ellipsis'"
            class="inline-flex size-8 items-center justify-center text-xs font-bold text-cp-muted-text"
          >
            …
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

        <BaseIconButton
          variant="secondary"
          size="sm"
          :disabled="loading || currentPage >= totalPages"
          label="下一页"
          @click="goToPage(currentPage + 1)"
        >
          <ChevronRight class="size-4" />
        </BaseIconButton>
      </div>
    </div>
  </footer>
</template>
