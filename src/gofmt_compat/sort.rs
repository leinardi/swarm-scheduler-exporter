/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

//! Go's `sort.Sort`, a pattern-defeating quicksort (go1.26.10 `sort/zsortinterface.go`), over a
//! slice and a `less` function.
//!
//! It exists for comparisons that are not a strict weak order, such as goautoneg's ordering of
//! `Accept` clauses: there the result depends on the exact sequence of comparisons and swaps, so
//! only Go's algorithm, step for step, gives Go's order. Up to 12 elements that is an insertion
//! sort; above it, pdqsort with its deterministic pattern breaking.

/// The hint `choose_pivot` gives about the order of the slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortedHint {
    Unknown,
    Increasing,
    Decreasing,
}

/// The slice and its comparison, with Go's `Interface` operations on indices.
struct Data<'a, T, F> {
    items: &'a mut [T],
    less: F,
}

impl<T, F: Fn(&T, &T) -> bool> Data<'_, T, F> {
    fn less(&self, left: usize, right: usize) -> bool {
        (self.less)(&self.items[left], &self.items[right])
    }

    fn swap(&mut self, left: usize, right: usize) {
        self.items.swap(left, right);
    }
}

/// `sort.Sort(data)`: sorts `items` in Go's order for `less`.
pub fn sort<T, F: Fn(&T, &T) -> bool>(items: &mut [T], less: F) {
    let length = items.len();
    if length <= 1 {
        return;
    }
    let limit = bit_length(length);
    let mut data = Data { items, less };
    pdqsort(&mut data, 0, length, limit);
}

/// `bits.Len`: the number of bits needed to represent `value`.
fn bit_length(value: usize) -> u32 {
    usize::BITS - value.leading_zeros()
}

fn insertion_sort<T, F: Fn(&T, &T) -> bool>(data: &mut Data<'_, T, F>, start: usize, end: usize) {
    for outer in start + 1..end {
        let mut inner = outer;
        while inner > start && data.less(inner, inner - 1) {
            data.swap(inner, inner - 1);
            inner -= 1;
        }
    }
}

/// `siftDown`: the heap property on `data[low:high]`, the root at offset `first`.
fn sift_down<T, F: Fn(&T, &T) -> bool>(
    data: &mut Data<'_, T, F>,
    low: usize,
    high: usize,
    first: usize,
) {
    let mut root = low;
    loop {
        let mut child = 2 * root + 1;
        if child >= high {
            break;
        }
        if child + 1 < high && data.less(first + child, first + child + 1) {
            child += 1;
        }
        if !data.less(first + root, first + child) {
            return;
        }
        data.swap(first + root, first + child);
        root = child;
    }
}

fn heap_sort<T, F: Fn(&T, &T) -> bool>(data: &mut Data<'_, T, F>, start: usize, end: usize) {
    let first = start;
    let high = end - start;
    // Build a heap with the greatest element at the top.
    for index in (0..=(high - 1) / 2).rev() {
        sift_down(data, index, high, first);
    }
    // Pop the elements, largest first, into the end.
    for index in (0..high).rev() {
        data.swap(first, first + index);
        sift_down(data, 0, index, first);
    }
}

/// `pdqsort`: sorts `data[start:end]`, falling back to heapsort after `limit` bad pivots.
fn pdqsort<T, F: Fn(&T, &T) -> bool>(
    data: &mut Data<'_, T, F>,
    mut start: usize,
    mut end: usize,
    mut limit: u32,
) {
    const MAX_INSERTION: usize = 12;
    // Whether the last partitioning was reasonably balanced, and whether the slice was already
    // partitioned.
    let mut was_balanced = true;
    let mut was_partitioned = true;
    loop {
        let length = end - start;
        if length <= MAX_INSERTION {
            insertion_sort(data, start, end);
            return;
        }
        // Too many bad choices: heapsort.
        if limit == 0 {
            heap_sort(data, start, end);
            return;
        }
        // An imbalanced last partitioning: break the patterns.
        if !was_balanced {
            break_patterns(data, start, end);
            limit -= 1;
        }
        let (mut pivot, mut hint) = choose_pivot(data, start, end);
        if hint == SortedHint::Decreasing {
            reverse_range(data, start, end);
            // The pivot was pivot - start after the start; after the reversal it is as far
            // before the end.
            pivot = (end - 1) - (pivot - start);
            hint = SortedHint::Increasing;
        }
        // Likely already sorted.
        if was_balanced
            && was_partitioned
            && hint == SortedHint::Increasing
            && partial_insertion_sort(data, start, end)
        {
            return;
        }
        // Likely many duplicates: put the elements equal to the pivot first.
        if start > 0 && !data.less(start - 1, pivot) {
            start = partition_equal(data, start, end, pivot);
            continue;
        }
        let (middle, already_partitioned) = partition(data, start, end, pivot);
        was_partitioned = already_partitioned;
        let (left_length, right_length) = (middle - start, end - middle);
        let balance_threshold = length / 8;
        if left_length < right_length {
            was_balanced = left_length >= balance_threshold;
            pdqsort(data, start, middle, limit);
            start = middle + 1;
        } else {
            was_balanced = right_length >= balance_threshold;
            pdqsort(data, middle + 1, end, limit);
            end = middle;
        }
    }
}

/// `partition`: one quicksort partition around `data[pivot]`, which ends at the returned index;
/// also whether nothing had to move.
fn partition<T, F: Fn(&T, &T) -> bool>(
    data: &mut Data<'_, T, F>,
    start: usize,
    end: usize,
    pivot: usize,
) -> (usize, bool) {
    data.swap(start, pivot);
    // Both inclusive of the elements still to partition.
    let (mut low, mut high) = (start + 1, end - 1);
    while low <= high && data.less(low, start) {
        low += 1;
    }
    while low <= high && !data.less(high, start) {
        high -= 1;
    }
    if low > high {
        data.swap(high, start);
        return (high, true);
    }
    data.swap(low, high);
    low += 1;
    high -= 1;
    loop {
        while low <= high && data.less(low, start) {
            low += 1;
        }
        while low <= high && !data.less(high, start) {
            high -= 1;
        }
        if low > high {
            break;
        }
        data.swap(low, high);
        low += 1;
        high -= 1;
    }
    data.swap(high, start);
    (high, false)
}

/// `partitionEqual`: the elements equal to `data[pivot]` first, then the greater ones; returns
/// where the greater ones start.
fn partition_equal<T, F: Fn(&T, &T) -> bool>(
    data: &mut Data<'_, T, F>,
    start: usize,
    end: usize,
    pivot: usize,
) -> usize {
    data.swap(start, pivot);
    let (mut low, mut high) = (start + 1, end - 1);
    loop {
        while low <= high && !data.less(start, low) {
            low += 1;
        }
        while low <= high && data.less(start, high) {
            high -= 1;
        }
        if low > high {
            break;
        }
        data.swap(low, high);
        low += 1;
        high -= 1;
    }
    low
}

/// `partialInsertionSort`: shifts a few out-of-order pairs; true if the slice ends up sorted.
fn partial_insertion_sort<T, F: Fn(&T, &T) -> bool>(
    data: &mut Data<'_, T, F>,
    start: usize,
    end: usize,
) -> bool {
    // The most adjacent out-of-order pairs shifted, and the shortest slice that shifts at all.
    const MAX_STEPS: usize = 5;
    const SHORTEST_SHIFTING: usize = 50;
    let mut index = start + 1;
    for _ in 0..MAX_STEPS {
        while index < end && !data.less(index, index - 1) {
            index += 1;
        }
        if index == end {
            return true;
        }
        if end - start < SHORTEST_SHIFTING {
            return false;
        }
        data.swap(index, index - 1);
        // Shift the smaller one to the left. Go's loop runs down to index 1, not start + 1.
        if index - start >= 2 {
            let mut shift = index - 1;
            while shift >= 1 {
                if !data.less(shift, shift - 1) {
                    break;
                }
                data.swap(shift, shift - 1);
                shift -= 1;
            }
        }
        // Shift the greater one to the right.
        if end - index >= 2 {
            for shift in index + 1..end {
                if !data.less(shift, shift - 1) {
                    break;
                }
                data.swap(shift, shift - 1);
            }
        }
    }
    false
}

/// `breakPatterns`: swaps three elements around the middle with pseudo-random ones, seeded by
/// the length, as Go's xorshift does.
fn break_patterns<T, F: Fn(&T, &T) -> bool>(data: &mut Data<'_, T, F>, start: usize, end: usize) {
    let length = end - start;
    if length >= 8 {
        let mut random = u64::try_from(length).unwrap_or(u64::MAX);
        let mask = u64::try_from((1_usize << bit_length(length)) - 1).unwrap_or(u64::MAX);
        let middle = start + (length / 4) * 2;
        for index in middle - 1..=middle + 1 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            let mut other = usize::try_from(random & mask).unwrap_or(0);
            if other >= length {
                other -= length;
            }
            data.swap(index, start + other);
        }
    }
}

/// `choosePivot`: a static pivot below 8 elements, the median of three below 50, Tukey's
/// ninther above; with the order the comparisons suggest.
fn choose_pivot<T, F: Fn(&T, &T) -> bool>(
    data: &mut Data<'_, T, F>,
    start: usize,
    end: usize,
) -> (usize, SortedHint) {
    const SHORTEST_NINTHER: usize = 50;
    const MAX_SWAPS: u32 = 4 * 3;
    let length = end - start;
    let mut swaps = 0;
    let mut first = start + length / 4;
    let mut second = start + length / 4 * 2;
    let mut third = start + length / 4 * 3;
    if length >= 8 {
        if length >= SHORTEST_NINTHER {
            first = median_adjacent(data, first, &mut swaps);
            second = median_adjacent(data, second, &mut swaps);
            third = median_adjacent(data, third, &mut swaps);
        }
        second = median(data, first, second, third, &mut swaps);
    }
    match swaps {
        0 => (second, SortedHint::Increasing),
        MAX_SWAPS => (second, SortedHint::Decreasing),
        _ => (second, SortedHint::Unknown),
    }
}

/// `order2`: the two indices, the one of the smaller element first.
fn order2<T, F: Fn(&T, &T) -> bool>(
    data: &Data<'_, T, F>,
    left: usize,
    right: usize,
    swaps: &mut u32,
) -> (usize, usize) {
    if data.less(right, left) {
        *swaps += 1;
        return (right, left);
    }
    (left, right)
}

/// `median`: the index of the median of three elements.
fn median<T, F: Fn(&T, &T) -> bool>(
    data: &Data<'_, T, F>,
    first: usize,
    second: usize,
    third: usize,
    swaps: &mut u32,
) -> usize {
    let (first, second) = order2(data, first, second, swaps);
    let (second, _third) = order2(data, second, third, swaps);
    let (_first, second) = order2(data, first, second, swaps);
    second
}

/// `medianAdjacent`: the median of the element at `index` and its two neighbours.
fn median_adjacent<T, F: Fn(&T, &T) -> bool>(
    data: &Data<'_, T, F>,
    index: usize,
    swaps: &mut u32,
) -> usize {
    median(data, index - 1, index, index + 1, swaps)
}

fn reverse_range<T, F: Fn(&T, &T) -> bool>(data: &mut Data<'_, T, F>, start: usize, end: usize) {
    let (mut low, mut high) = (start, end - 1);
    while low < high {
        data.swap(low, high);
        low += 1;
        high -= 1;
    }
}
