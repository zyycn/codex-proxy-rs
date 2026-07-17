import type { Ref } from 'vue'
import type BaseScrollbar from '../BaseScrollbar.vue'

import { useResizeObserver } from '@vueuse/core'
import { nextTick, onMounted, shallowRef, useTemplateRef, watch } from 'vue'

interface UseHorizontalStickyShadowOptions {
  hasRows: Ref<boolean>
  watchSources: () => unknown[]
}

export function useHorizontalStickyShadow(options: UseHorizontalStickyShadowOptions) {
  const headerWrapRef = useTemplateRef<HTMLDivElement>('headerWrap')
  const bodyScrollbarRef = useTemplateRef<InstanceType<typeof BaseScrollbar>>('bodyScrollbar')
  const tableViewRef = useTemplateRef<HTMLTableElement>('tableView')
  const horizontalScrolled = shallowRef(false)
  const horizontalCanScrollRight = shallowRef(false)

  function maxScrollLeft(wrap: HTMLElement) {
    return Math.max(wrap.scrollWidth - wrap.clientWidth, 0)
  }

  function scrollWrap() {
    return bodyScrollbarRef.value?.wrapRef ?? null
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
    if (wrap && headerWrapRef.value) {
      headerWrapRef.value.scrollLeft = wrap.scrollLeft
    }
    updateHorizontalScrollbar()
  }

  onMounted(async () => {
    await nextTick()
    updateHorizontalScrollbar()
  })

  useResizeObserver(
    () =>
      [scrollWrap(), tableViewRef.value].filter(
        (element): element is HTMLDivElement | HTMLTableElement => Boolean(element),
      ),
    updateHorizontalScrollbar,
  )

  watch(options.watchSources, async () => {
    await nextTick()
    updateHorizontalScrollbar()
  })

  return {
    headerWrapRef,
    bodyScrollbarRef,
    tableViewRef,
    horizontalScrolled,
    horizontalCanScrollRight,
    handleTableScroll,
  }
}
