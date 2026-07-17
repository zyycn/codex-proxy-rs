import type { Ref } from 'vue'
import type BaseScrollbar from '../BaseScrollbar.vue'

import { useResizeObserver } from '@vueuse/core'
import { nextTick, onMounted, shallowRef, watch } from 'vue'

interface UseHorizontalStickyShadowOptions {
  hasRows: Ref<boolean>
  headerWrapRef: Readonly<Ref<HTMLDivElement | null>>
  bodyScrollbarRef: Readonly<Ref<InstanceType<typeof BaseScrollbar> | null>>
  tableViewRef: Readonly<Ref<HTMLTableElement | null>>
  watchSources: () => unknown[]
}

export function useHorizontalStickyShadow(options: UseHorizontalStickyShadowOptions) {
  const horizontalScrolled = shallowRef(false)
  const horizontalCanScrollRight = shallowRef(false)

  function maxScrollLeft(wrap: HTMLElement) {
    return Math.max(wrap.scrollWidth - wrap.clientWidth, 0)
  }

  function scrollWrap() {
    return options.bodyScrollbarRef.value?.wrapRef ?? null
  }

  function resetHorizontalScrollbar() {
    horizontalScrolled.value = false
    horizontalCanScrollRight.value = false
  }

  function updateHorizontalScrollbar() {
    const wrap = scrollWrap()
    if (!wrap || !options.hasRows.value) {
      resetHorizontalScrollbar()
      return
    }

    const scrollRange = maxScrollLeft(wrap)
    if (scrollRange <= 0) {
      resetHorizontalScrollbar()
      return
    }

    horizontalScrolled.value = wrap.scrollLeft > 0
    horizontalCanScrollRight.value = wrap.scrollLeft < scrollRange - 1
  }

  function handleTableScroll() {
    const wrap = scrollWrap()
    if (wrap && options.headerWrapRef.value) {
      options.headerWrapRef.value.scrollLeft = wrap.scrollLeft
    }
    updateHorizontalScrollbar()
  }

  onMounted(async () => {
    await nextTick()
    updateHorizontalScrollbar()
  })

  useResizeObserver(
    () =>
      [scrollWrap(), options.tableViewRef.value].filter(
        (element): element is HTMLDivElement | HTMLTableElement => Boolean(element),
      ),
    updateHorizontalScrollbar,
  )

  watch(options.watchSources, async () => {
    await nextTick()
    updateHorizontalScrollbar()
  })

  return {
    horizontalScrolled,
    horizontalCanScrollRight,
    handleTableScroll,
  }
}
