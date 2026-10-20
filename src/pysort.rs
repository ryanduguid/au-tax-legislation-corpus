//! CPython 3.15's `list.sort` (Objects/listobject.c): the same comparisons in
//! the same order, so values that do not order raise the TypeError Python's
//! sort meets first, and a comparison that is not a total order (NaN) leaves
//! the items where Python leaves them.
//!
//! `lt(a, b)` is `a < b`. On error the order of `items` is unchanged. The sort
//! moves positions, not items, then swaps each item into place once, so no
//! item is ever cloned.

const MAX_MINRUN: usize = 64;
const MIN_GALLOP: usize = 7;

struct Pending {
    base: usize,
    len: usize,
    power: u32,
}

/// The positions being sorted and `lt` over the positions' items.
struct Sorter<'a, F> {
    a: &'a mut [usize],
    lt: F,
    min_gallop: usize,
    pending: Vec<Pending>,
}

/// `items.sort()` with `lt` as `<`.
pub fn sort<T, E>(items: &mut [T], mut lt: impl FnMut(&T, &T) -> Result<bool, E>) -> Result<(), E> {
    let mut order: Vec<usize> = (0..items.len()).collect();
    let view: &[T] = items;
    sort_positions(&mut order, |i, j| lt(&view[i], &view[j]))?;
    // Item `order[k]` belongs at `k`: follow each cycle of that permutation.
    let mut placed = vec![false; order.len()];
    for start in 0..order.len() {
        let mut k = start;
        while !placed[k] {
            placed[k] = true;
            let source = order[k];
            if source == start {
                break;
            }
            items.swap(k, source);
            k = source;
        }
    }
    Ok(())
}

fn sort_positions<E>(
    order: &mut [usize],
    lt: impl FnMut(usize, usize) -> Result<bool, E>,
) -> Result<(), E> {
    let listlen = order.len();
    if listlen < 2 {
        return Ok(());
    }
    let mut s = Sorter {
        a: order,
        lt,
        min_gallop: MIN_GALLOP,
        pending: Vec::new(),
    };
    // minrun state: the ideal run length is listlen / 2**mr_e.
    let mut mr_e = 0;
    while listlen >> mr_e >= MAX_MINRUN {
        mr_e += 1;
    }
    let mr_mask = (1usize << mr_e) - 1;
    let mut mr_current = 0usize;

    let (mut lo, mut nremaining) = (0usize, listlen);
    while nremaining > 0 {
        let mut n = s.count_run(lo, nremaining)?;
        mr_current += listlen;
        let minrun = mr_current >> mr_e;
        mr_current &= mr_mask;
        if n < minrun {
            let force = nremaining.min(minrun);
            s.binarysort(lo, force, n)?;
            n = force;
        }
        s.found_new_run(n, listlen)?;
        s.pending.push(Pending {
            base: lo,
            len: n,
            power: 0,
        });
        lo += n;
        nremaining -= n;
    }
    s.merge_force_collapse()
}

/// Where `key` goes in the sorted `v`, left of any equal item.
fn gallop_left<E>(
    lt: &mut impl FnMut(usize, usize) -> Result<bool, E>,
    key: usize,
    v: &[usize],
    hint: usize,
) -> Result<usize, E> {
    let n = v.len() as isize;
    let hint = hint as isize;
    let (mut lastofs, mut ofs): (isize, isize) = (0, 1);
    if lt(v[hint as usize], key)? {
        let maxofs = n - hint;
        while ofs < maxofs {
            if lt(v[(hint + ofs) as usize], key)? {
                lastofs = ofs;
                ofs = (ofs << 1) + 1;
            } else {
                break;
            }
        }
        ofs = ofs.min(maxofs);
        lastofs += hint;
        ofs += hint;
    } else {
        let maxofs = hint + 1;
        while ofs < maxofs {
            if lt(v[(hint - ofs) as usize], key)? {
                break;
            }
            lastofs = ofs;
            ofs = (ofs << 1) + 1;
        }
        ofs = ofs.min(maxofs);
        let k = lastofs;
        lastofs = hint - ofs;
        ofs = hint - k;
    }
    lastofs += 1;
    while lastofs < ofs {
        let m = lastofs + ((ofs - lastofs) >> 1);
        if lt(v[m as usize], key)? {
            lastofs = m + 1;
        } else {
            ofs = m;
        }
    }
    Ok(ofs as usize)
}

/// Where `key` goes in the sorted `v`, right of any equal item.
fn gallop_right<E>(
    lt: &mut impl FnMut(usize, usize) -> Result<bool, E>,
    key: usize,
    v: &[usize],
    hint: usize,
) -> Result<usize, E> {
    let n = v.len() as isize;
    let hint = hint as isize;
    let (mut lastofs, mut ofs): (isize, isize) = (0, 1);
    if lt(key, v[hint as usize])? {
        let maxofs = hint + 1;
        while ofs < maxofs {
            if lt(key, v[(hint - ofs) as usize])? {
                lastofs = ofs;
                ofs = (ofs << 1) + 1;
            } else {
                break;
            }
        }
        ofs = ofs.min(maxofs);
        let k = lastofs;
        lastofs = hint - ofs;
        ofs = hint - k;
    } else {
        let maxofs = n - hint;
        while ofs < maxofs {
            if lt(key, v[(hint + ofs) as usize])? {
                break;
            }
            lastofs = ofs;
            ofs = (ofs << 1) + 1;
        }
        ofs = ofs.min(maxofs);
        lastofs += hint;
        ofs += hint;
    }
    lastofs += 1;
    while lastofs < ofs {
        let m = lastofs + ((ofs - lastofs) >> 1);
        if lt(key, v[m as usize])? {
            ofs = m;
        } else {
            lastofs = m + 1;
        }
    }
    Ok(ofs as usize)
}

impl<E, F: FnMut(usize, usize) -> Result<bool, E>> Sorter<'_, F> {
    /// `lt(a[i], a[j])`.
    fn lt_at(&mut self, i: usize, j: usize) -> Result<bool, E> {
        (self.lt)(self.a[i], self.a[j])
    }

    /// Stable binary insertion sort of `a[lo..lo+n]`, of which `a[lo..lo+ok]`
    /// is already sorted.
    fn binarysort(&mut self, lo: usize, n: usize, ok: usize) -> Result<(), E> {
        let mut ok = ok.max(1);
        while ok < n {
            let (mut l, mut r) = (0, ok);
            loop {
                let m = (l + r) >> 1;
                if self.lt_at(lo + ok, lo + m)? {
                    r = m;
                } else {
                    l = m + 1;
                }
                if l >= r {
                    break;
                }
            }
            self.a[lo + l..=lo + ok].rotate_right(1);
            ok += 1;
        }
        Ok(())
    }

    /// The length of the run starting at `a[lo]`, reversed in place to
    /// ascending if it descends.
    fn count_run(&mut self, lo: usize, nremaining: usize) -> Result<usize, E> {
        let mut n = 1;
        while n < nremaining {
            if self.lt_at(lo + n, lo + n - 1)? {
                break;
            }
            n += 1;
        }
        if n == nremaining {
            return Ok(n);
        }
        if n > 1 {
            if self.lt_at(lo, lo + n - 1)? {
                return Ok(n);
            }
            self.a[lo..lo + n].reverse();
        }
        n += 1;
        let mut neq = 0;
        let reverse_last_neq = |a: &mut [usize], n: usize, neq: &mut usize| {
            if *neq > 0 {
                *neq += 1;
                a[lo + n - *neq..lo + n].reverse();
                *neq = 0;
            }
        };
        while n < nremaining {
            if self.lt_at(lo + n, lo + n - 1)? {
                reverse_last_neq(self.a, n, &mut neq);
            } else if self.lt_at(lo + n - 1, lo + n)? {
                break;
            } else {
                neq += 1;
            }
            n += 1;
        }
        reverse_last_neq(self.a, n, &mut neq);
        self.a[lo..lo + n].reverse();
        while n < nremaining {
            if self.lt_at(lo + n, lo + n - 1)? {
                break;
            }
            n += 1;
        }
        Ok(n)
    }

    /// Merge `a[base_a..base_a+na]` and the run after it, `na <= nb`, through
    /// a copy of the first.
    fn merge_lo(
        &mut self,
        base_a: usize,
        mut na: usize,
        base_b: usize,
        mut nb: usize,
    ) -> Result<(), E> {
        let tmp: Vec<usize> = self.a[base_a..base_a + na].to_vec();
        let (mut dest, mut pa, mut pb) = (base_a, 0usize, base_b);
        let copy_b = |a: &mut [usize], dest: usize, pb: usize, nb: usize, last: usize| {
            a.copy_within(pb..pb + nb, dest);
            a[dest + nb] = last;
        };
        self.a[dest] = self.a[pb];
        dest += 1;
        pb += 1;
        nb -= 1;
        if nb == 0 {
            self.a[dest..dest + na].copy_from_slice(&tmp[pa..pa + na]);
            return Ok(());
        }
        if na == 1 {
            copy_b(self.a, dest, pb, nb, tmp[pa]);
            return Ok(());
        }
        let mut min_gallop = self.min_gallop;
        loop {
            let (mut acount, mut bcount) = (0usize, 0usize);
            loop {
                if (self.lt)(self.a[pb], tmp[pa])? {
                    self.a[dest] = self.a[pb];
                    dest += 1;
                    pb += 1;
                    bcount += 1;
                    acount = 0;
                    nb -= 1;
                    if nb == 0 {
                        self.a[dest..dest + na].copy_from_slice(&tmp[pa..pa + na]);
                        return Ok(());
                    }
                    if bcount >= min_gallop {
                        break;
                    }
                } else {
                    self.a[dest] = tmp[pa];
                    dest += 1;
                    pa += 1;
                    acount += 1;
                    bcount = 0;
                    na -= 1;
                    if na == 1 {
                        copy_b(self.a, dest, pb, nb, tmp[pa]);
                        return Ok(());
                    }
                    if acount >= min_gallop {
                        break;
                    }
                }
            }
            min_gallop += 1;
            loop {
                min_gallop -= usize::from(min_gallop > 1);
                self.min_gallop = min_gallop;
                let k = gallop_right(&mut self.lt, self.a[pb], &tmp[pa..pa + na], 0)?;
                acount = k;
                if k > 0 {
                    self.a[dest..dest + k].copy_from_slice(&tmp[pa..pa + k]);
                    dest += k;
                    pa += k;
                    na -= k;
                    if na == 1 {
                        copy_b(self.a, dest, pb, nb, tmp[pa]);
                        return Ok(());
                    }
                    if na == 0 {
                        return Ok(());
                    }
                }
                self.a[dest] = self.a[pb];
                dest += 1;
                pb += 1;
                nb -= 1;
                if nb == 0 {
                    self.a[dest..dest + na].copy_from_slice(&tmp[pa..pa + na]);
                    return Ok(());
                }
                let k = gallop_left(&mut self.lt, tmp[pa], &self.a[pb..pb + nb], 0)?;
                bcount = k;
                if k > 0 {
                    self.a.copy_within(pb..pb + k, dest);
                    dest += k;
                    pb += k;
                    nb -= k;
                    if nb == 0 {
                        self.a[dest..dest + na].copy_from_slice(&tmp[pa..pa + na]);
                        return Ok(());
                    }
                }
                self.a[dest] = tmp[pa];
                dest += 1;
                pa += 1;
                na -= 1;
                if na == 1 {
                    copy_b(self.a, dest, pb, nb, tmp[pa]);
                    return Ok(());
                }
                if acount < MIN_GALLOP && bcount < MIN_GALLOP {
                    break;
                }
            }
            min_gallop += 1;
            self.min_gallop = min_gallop;
        }
    }

    /// Merge `a[base_a..base_a+na]` and the run after it, `na >= nb`, from
    /// the right through a copy of the second.
    fn merge_hi(
        &mut self,
        base_a: usize,
        mut na: usize,
        base_b: usize,
        mut nb: usize,
    ) -> Result<(), E> {
        let tmp: Vec<usize> = self.a[base_b..base_b + nb].to_vec();
        // Signed, as CPython's pointers may step one before a run.
        let mut dest = (base_b + nb - 1) as isize;
        let mut pa = (base_a + na - 1) as isize;
        let mut pb = (nb - 1) as isize;
        let at = |i: isize| i as usize;
        // On success with b left over: tmp[..nb] fills a[dest-(nb-1)..=dest].
        let fill_b = |a: &mut [usize], dest: isize, nb: usize| {
            let start = at(dest + 1) - nb;
            a[start..start + nb].copy_from_slice(&tmp[..nb]);
        };
        // b's first item goes before what remains of a, shifted right.
        let copy_a = |a: &mut [usize], dest: isize, pa: isize, na: usize, first: usize| {
            a.copy_within(at(pa) + 1 - na..=at(pa), at(dest) + 1 - na);
            a[at(dest) - na] = first;
        };
        self.a[at(dest)] = self.a[at(pa)];
        dest -= 1;
        pa -= 1;
        na -= 1;
        if na == 0 {
            fill_b(self.a, dest, nb);
            return Ok(());
        }
        if nb == 1 {
            copy_a(self.a, dest, pa, na, tmp[at(pb)]);
            return Ok(());
        }
        let mut min_gallop = self.min_gallop;
        loop {
            let (mut acount, mut bcount) = (0usize, 0usize);
            loop {
                if (self.lt)(tmp[at(pb)], self.a[at(pa)])? {
                    self.a[at(dest)] = self.a[at(pa)];
                    dest -= 1;
                    pa -= 1;
                    acount += 1;
                    bcount = 0;
                    na -= 1;
                    if na == 0 {
                        fill_b(self.a, dest, nb);
                        return Ok(());
                    }
                    if acount >= min_gallop {
                        break;
                    }
                } else {
                    self.a[at(dest)] = tmp[at(pb)];
                    dest -= 1;
                    pb -= 1;
                    bcount += 1;
                    acount = 0;
                    nb -= 1;
                    if nb == 1 {
                        copy_a(self.a, dest, pa, na, tmp[at(pb)]);
                        return Ok(());
                    }
                    if bcount >= min_gallop {
                        break;
                    }
                }
            }
            min_gallop += 1;
            loop {
                min_gallop -= usize::from(min_gallop > 1);
                self.min_gallop = min_gallop;
                let key = tmp[at(pb)];
                let k = na - gallop_right(&mut self.lt, key, &self.a[base_a..base_a + na], na - 1)?;
                acount = k;
                if k > 0 {
                    dest -= k as isize;
                    pa -= k as isize;
                    self.a.copy_within(at(pa + 1)..at(pa + 1) + k, at(dest + 1));
                    na -= k;
                    if na == 0 {
                        fill_b(self.a, dest, nb);
                        return Ok(());
                    }
                }
                self.a[at(dest)] = tmp[at(pb)];
                dest -= 1;
                pb -= 1;
                nb -= 1;
                if nb == 1 {
                    copy_a(self.a, dest, pa, na, tmp[at(pb)]);
                    return Ok(());
                }
                let k = nb - gallop_left(&mut self.lt, self.a[at(pa)], &tmp[..nb], nb - 1)?;
                bcount = k;
                if k > 0 {
                    dest -= k as isize;
                    pb -= k as isize;
                    let start = at(pb + 1);
                    self.a[at(dest + 1)..at(dest + 1) + k].copy_from_slice(&tmp[start..start + k]);
                    nb -= k;
                    if nb == 1 {
                        copy_a(self.a, dest, pa, na, tmp[at(pb)]);
                        return Ok(());
                    }
                    if nb == 0 {
                        return Ok(());
                    }
                }
                self.a[at(dest)] = self.a[at(pa)];
                dest -= 1;
                pa -= 1;
                na -= 1;
                if na == 0 {
                    fill_b(self.a, dest, nb);
                    return Ok(());
                }
                if acount < MIN_GALLOP && bcount < MIN_GALLOP {
                    break;
                }
            }
            min_gallop += 1;
            self.min_gallop = min_gallop;
        }
    }

    /// Merge the pending runs at `i` and `i + 1`.
    fn merge_at(&mut self, i: usize) -> Result<(), E> {
        let (mut base_a, mut na) = (self.pending[i].base, self.pending[i].len);
        let (base_b, nb) = (self.pending[i + 1].base, self.pending[i + 1].len);
        self.pending[i].len = na + nb;
        self.pending.remove(i + 1);

        let k = gallop_right(
            &mut self.lt,
            self.a[base_b],
            &self.a[base_a..base_a + na],
            0,
        )?;
        base_a += k;
        na -= k;
        if na == 0 {
            return Ok(());
        }
        let key = self.a[base_a + na - 1];
        let nb = gallop_left(&mut self.lt, key, &self.a[base_b..base_b + nb], nb - 1)?;
        if nb == 0 {
            return Ok(());
        }
        if na <= nb {
            self.merge_lo(base_a, na, base_b, nb)
        } else {
            self.merge_hi(base_a, na, base_b, nb)
        }
    }

    /// Powersort: merge pending runs deeper than the one the new run of
    /// length `n2` closes.
    fn found_new_run(&mut self, n2: usize, listlen: usize) -> Result<(), E> {
        let Some(last) = self.pending.last() else {
            return Ok(());
        };
        let power = powerloop(last.base, last.len, n2, listlen);
        while self.pending.len() > 1 && self.pending[self.pending.len() - 2].power > power {
            self.merge_at(self.pending.len() - 2)?;
        }
        if let Some(last) = self.pending.last_mut() {
            last.power = power;
        }
        Ok(())
    }

    fn merge_force_collapse(&mut self) -> Result<(), E> {
        while self.pending.len() > 1 {
            let mut n = self.pending.len() - 2;
            if n > 0 && self.pending[n - 1].len < self.pending[n + 1].len {
                n -= 1;
            }
            self.merge_at(n)?;
        }
        Ok(())
    }
}

/// The `python_sort` fuzz target's invariants for one input: under a total
/// order the sort is the stable sort, and under one that is not (NaN) it
/// still returns every item once. The unit tests replay seeds through this.
#[doc(hidden)]
pub fn check_one_input(data: &[u8]) -> Result<(), String> {
    let mut items: Vec<(u8, usize)> = data.iter().enumerate().map(|(i, &b)| (b % 16, i)).collect();
    let mut expected = items.clone();
    expected.sort_by_key(|item| item.0);
    sort(&mut items, |a, b| Ok::<_, ()>(a.0 < b.0)).map_err(|()| "no comparison fails")?;
    if items != expected {
        return Err(format!("{items:?} is not the stable sort {expected:?}"));
    }
    let mut floats: Vec<(f64, usize)> = data
        .iter()
        .enumerate()
        .map(|(i, &b)| (if b > 240 { f64::NAN } else { f64::from(b % 32) }, i))
        .collect();
    sort(&mut floats, |a, b| Ok::<_, ()>(a.0 < b.0)).map_err(|()| "no comparison fails")?;
    let mut seen: Vec<usize> = floats.iter().map(|item| item.1).collect();
    seen.sort_unstable();
    if !seen.iter().copied().eq(0..data.len()) {
        return Err("the sort lost or repeated an item".into());
    }
    Ok(())
}

/// The power of the run at `s1` of length `n1`, followed by one of length `n2`.
fn powerloop(s1: usize, n1: usize, n2: usize, n: usize) -> u32 {
    let mut result = 0;
    let mut a = 2 * s1 + n1;
    let mut b = a + n1 + n2;
    loop {
        result += 1;
        if a >= n {
            a -= n;
            b -= n;
        } else if b >= n {
            break;
        }
        a <<= 1;
        b <<= 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::sort;

    fn sorted(mut v: Vec<i64>) -> Vec<i64> {
        sort(&mut v, |a, b| Ok::<_, ()>(a < b)).unwrap();
        v
    }

    #[test]
    fn sorts_like_a_stable_sort_at_every_size() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for len in (0..300).chain([1000, 4096]) {
            let v: Vec<i64> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    (seed % 50) as i64
                })
                .collect();
            let mut expected = v.clone();
            expected.sort();
            assert_eq!(sorted(v), expected, "length {len}");
        }
    }

    /// CPython 3.15's `sorted(range(n), key=v.__getitem__)` over floats with
    /// NaNs, which compare false both ways, as FNV-1a of the comma-joined
    /// order; the 300 and 1000 cases merge and gallop.
    #[test]
    fn leaves_nans_where_cpython_leaves_them() {
        for (n, modulus, nan_every, expected) in [
            (40, 13, 7, 0xd17a_35f2_6c7a_9481u64),
            (300, 101, 17, 0x4e6b_75df_7be6_1367),
            (1000, 997, 29, 0x1dd6_cd0a_eab5_b237),
        ] {
            let v: Vec<f64> = (0..n)
                .map(|i| {
                    if i % nan_every == 3 {
                        f64::NAN
                    } else {
                        ((i * 7919) % modulus) as f64
                    }
                })
                .collect();
            let mut order: Vec<usize> = (0..n).collect();
            sort(&mut order, |&a, &b| Ok::<_, ()>(v[a] < v[b])).unwrap();
            let text = order
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let fnv = text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
            });
            assert_eq!(fnv, expected, "length {n}: {text}");
        }
    }

    #[test]
    fn stops_at_the_first_comparison_that_fails() {
        // [42, True, "C"]: the descending run meets "C" < True first.
        #[derive(Clone, Debug)]
        enum V {
            Int(i64),
            Str,
        }
        let mut v = vec![V::Int(42), V::Int(1), V::Str];
        let error = sort(&mut v, |a, b| match (a, b) {
            (V::Int(x), V::Int(y)) => Ok(x < y),
            (V::Str, V::Int(y)) => Err(format!("str < {y}")),
            (V::Int(x), V::Str) => Err(format!("{x} < str")),
            (V::Str, V::Str) => Ok(false),
        })
        .unwrap_err();
        assert_eq!(error, "str < 1");
    }

    #[test]
    fn is_stable() {
        let mut v: Vec<(u8, usize)> = (0..500).map(|i| ((i * 7 % 5) as u8, i)).collect();
        let mut expected = v.clone();
        expected.sort_by_key(|p| p.0);
        sort(&mut v, |a, b| Ok::<_, ()>(a.0 < b.0)).unwrap();
        assert_eq!(v, expected);
    }

    /// Items are only swapped: a type that cannot be cloned sorts, stably.
    #[test]
    fn sorts_items_that_cannot_be_cloned() {
        struct Item(u32, usize);
        let mut v: Vec<Item> = (0..2000)
            .map(|i| Item((i as u32 * 7919) % 1009, i))
            .collect();
        sort(&mut v, |a, b| Ok::<_, ()>(a.0 < b.0)).unwrap();
        assert!(v.windows(2).all(|w| (w[0].0, w[0].1) < (w[1].0, w[1].1)));
    }
}
