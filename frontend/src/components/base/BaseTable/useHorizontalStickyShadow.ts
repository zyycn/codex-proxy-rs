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
  let horizontalScrollRange = 0
  let scrollLeftPosition = 0

  function maxScrollLeft(wrap: HTMLElement) {
    return Math.max(wrap.scrollWidth - wrap.clientWidth, 0)
  }

  function scrollWrap() {
    return options.bodyScrollbarRef.value?.wrapRef ?? null
  }

  function resetHorizontalScrollbar() {
    horizontalScrollRange = 0
    scrollLeftPosition = 0
    horizontalScrolled.value = false
    horizontalCanScrollRight.value = false
    syncHeaderScroll(0)
  }

  function syncHeaderScroll(scrollLeft: number) {
    const headerWrap = options.headerWrapRef.value
    if (headerWrap && headerWrap.scrollLeft !== scrollLeft) {
      headerWrap.scrollLeft = scrollLeft
    }
  }

  function updateHorizontalScrollPosition(scrollLeft: number) {
    horizontalScrolled.value = scrollLeft > 0
    horizontalCanScrollRight.value = scrollLeft < horizontalScrollRange - 1
  }

  function measureHorizontalScrollbar() {
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

    horizontalScrollRange = scrollRange
    scrollLeftPosition = wrap.scrollLeft
    syncHeaderScroll(scrollLeftPosition)
    updateHorizontalScrollPosition(scrollLeftPosition)
  }

  function handleTableScroll(payload: { scrollTop: number, scrollLeft: number }) {
    if (payload.scrollLeft === scrollLeftPosition) {
      return
    }

    scrollLeftPosition = payload.scrollLeft
    syncHeaderScroll(scrollLeftPosition)
    updateHorizontalScrollPosition(scrollLeftPosition)
  }

  onMounted(async () => {
    await nextTick()
    measureHorizontalScrollbar()
  })

  useResizeObserver(
    () =>
      [scrollWrap(), options.tableViewRef.value].filter(
        (element): element is HTMLDivElement | HTMLTableElement => Boolean(element),
      ),
    measureHorizontalScrollbar,
  )

  watch(options.watchSources, async () => {
    await nextTick()
    measureHorizontalScrollbar()
  })

  return {
    horizontalScrolled,
    horizontalCanScrollRight,
    handleTableScroll,
  }
}
