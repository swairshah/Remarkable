// CurlPager.swift — the Apple Books page turn, for real.
//
// Instead of faking a bend with rotation3DEffect, this wraps
// UIPageViewController(transitionStyle: .pageCurl) — the exact UIKit
// machinery iBooks shipped with. The paper deforms and peels under the
// finger, casts the moving fold shadow, and springs to commit or cancel
// on release. All system-drawn.
//
// Tone-true backsides: the deck runs DOUBLE-SIDED, interleaving each
// page with a caller-supplied back view (paper-tone colored). Without
// this, UIKit invents the back as a white-washed mirror of the front —
// visibly wrong on Paper/Soft tones and in dark mode.
//
// Contract with DocumentView:
//   • `index` is the single source of truth. A finger curl updates the
//     binding when the turn commits; a programmatic change of `index`
//     (chevrons, goto, pi, add-page) plays the same animation.
//   • Pages are cached UIHostingControllers keyed by `pageKey` (inkKey),
//     so a page's PKCanvasView survives every turn — the same identity
//     discipline the old always-mounted deck enforced to stay flash-free.
//   • The curl's pan recognizer is DIRECT-TOUCH ONLY: Apple Pencil can
//     never turn a page, same rule as the old finger-only pager.
//   • `style` is fixed at creation; DocumentView re-ids the pager when
//     the Settings choice changes.

import SwiftUI
import UIKit

/// Settings > Appearance > Page turn.
enum PageTurnStyle: String, CaseIterable, Identifiable {
    case curl   // Books-style bending paper
    case slide  // flat side-scroll, no curl

    var id: String { rawValue }

    var label: String {
        switch self {
        case .curl: return "Curl"
        case .slide: return "Slide"
        }
    }
}

struct CurlPager<Page: View, Back: View>: UIViewControllerRepresentable {
    let style: PageTurnStyle
    let count: Int
    @Binding var index: Int
    let pageKey: (Int) -> String
    let makePage: (Int) -> Page
    /// The reverse side of a leaf, shown mid-curl. Paper tone lives here.
    let makeBack: (Int) -> Back
    /// When true (finger-draw mode), a curl may only START near the left or
    /// right page edge — mid-page finger drags belong to drawing. Books does
    /// the same: grab the margin to turn, touch the middle to interact.
    let edgeOnlyCurl: Bool

    func makeUIViewController(context: Context) -> UIPageViewController {
        let pager: UIPageViewController
        switch style {
        case .curl:
            pager = UIPageViewController(
                transitionStyle: .pageCurl,
                navigationOrientation: .horizontal,
                options: [.spineLocation: NSNumber(value: UIPageViewController.SpineLocation.min.rawValue)])
            // Double-sided + our own back controllers = the flip side is OUR
            // paper tone, not UIKit's white-washed mirror of the front.
            pager.isDoubleSided = true
        case .slide:
            pager = UIPageViewController(
                transitionStyle: .scroll,
                navigationOrientation: .horizontal,
                options: [.interPageSpacing: NSNumber(value: 24)])
        }
        pager.dataSource = context.coordinator
        pager.delegate = context.coordinator
        pager.view.backgroundColor = .clear

        // .pageCurl exposes its recognizers; .scroll manages its own pan.
        for recognizer in pager.gestureRecognizers {
            recognizer.allowedTouchTypes = [NSNumber(value: UITouch.TouchType.direct.rawValue)]
            if let pan = recognizer as? UIPanGestureRecognizer {
                pan.maximumNumberOfTouches = 1
                pan.delegate = context.coordinator
            }
            // No tap-to-turn: on a canvas you write on, an incidental finger
            // tap must never flip the page. The drag is the only turn.
            if recognizer is UITapGestureRecognizer {
                recognizer.isEnabled = false
            }
        }

        if let first = context.coordinator.frontController(at: index) {
            pager.setViewControllers([first], direction: .forward, animated: false)
        }
        return pager
    }

    func updateUIViewController(_ pager: UIPageViewController, context: Context) {
        let coordinator = context.coordinator
        coordinator.parent = self
        coordinator.refreshCachedControllers()
        coordinator.neuterPopGesture(around: pager)
        coordinator.sync(pager)
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    final class Coordinator: NSObject, UIPageViewControllerDataSource,
                             UIPageViewControllerDelegate, UIGestureRecognizerDelegate {
        var parent: CurlPager
        private var fronts: [String: UIHostingController<Page>] = [:]
        private var backs: [String: UIHostingController<Back>] = [:]
        private var gestureActive = false
        private var settling = false

        init(_ parent: CurlPager) { self.parent = parent }

        // MARK: leaf positions
        //
        // Double-sided order is [front 0, back 0, front 1, back 1, …]:
        // the back of leaf i sits between page i and page i+1.

        private enum Role {
            case front(Int)
            case back(Int)
        }

        private func position(ofKey key: String) -> Int? {
            (0..<parent.count).first { parent.pageKey($0) == key }
        }

        private func role(of controller: UIViewController) -> Role? {
            if let key = fronts.first(where: { $0.value === controller })?.key,
               let i = position(ofKey: key) {
                return .front(i)
            }
            if let key = backs.first(where: { $0.value === controller })?.key,
               let i = position(ofKey: key) {
                return .back(i)
            }
            return nil
        }

        func frontController(at index: Int) -> UIViewController? {
            guard index >= 0, index < parent.count else { return nil }
            let key = parent.pageKey(index)
            if let cached = fronts[key] {
                cached.rootView = parent.makePage(index)
                return cached
            }
            let hosting = UIHostingController(rootView: parent.makePage(index))
            hosting.view.backgroundColor = .clear
            hosting.safeAreaRegions = []
            hosting.loadViewIfNeeded()
            fronts[key] = hosting
            return hosting
        }

        private func backController(at index: Int) -> UIViewController? {
            guard parent.style == .curl, index >= 0, index < parent.count else { return nil }
            let key = parent.pageKey(index)
            if let cached = backs[key] {
                cached.rootView = parent.makeBack(index)
                return cached
            }
            let hosting = UIHostingController(rootView: parent.makeBack(index))
            hosting.view.backgroundColor = .clear
            hosting.safeAreaRegions = []
            hosting.loadViewIfNeeded()
            backs[key] = hosting
            return hosting
        }

        private func visibleIndex(of pager: UIPageViewController) -> Int? {
            guard let top = pager.viewControllers?.first, let role = role(of: top) else { return nil }
            switch role {
            case .front(let i): return i
            // Resting on a back never happens with a well-formed data
            // source; defensively treat it as the leaf just turned past.
            case .back(let i): return min(i + 1, parent.count - 1)
            }
        }

        /// Push the freshest SwiftUI state (tool, active flags, paper tone,
        /// seq shifts) into every retained page; drop pages that left the
        /// sequence.
        func refreshCachedControllers() {
            for (key, hosting) in fronts {
                if let i = position(ofKey: key) {
                    hosting.rootView = parent.makePage(i)
                } else {
                    fronts.removeValue(forKey: key)
                }
            }
            for (key, hosting) in backs {
                if let i = position(ofKey: key) {
                    hosting.rootView = parent.makeBack(i)
                } else {
                    backs.removeValue(forKey: key)
                }
            }
        }

        /// What an ANIMATED transition must be handed: a double-sided curl
        /// deck demands the whole leaf — front AND back — or UIKit throws
        /// "provided (1) doesn't match the number required (2)".
        private func animatedLeaf(at index: Int) -> [UIViewController]? {
            guard let front = frontController(at: index) else { return nil }
            if parent.style == .curl, let back = backController(at: index) {
                return [front, back]
            }
            return [front]
        }

        /// Programmatic navigation: when the binding disagrees with the
        /// visible page (and no finger owns the deck), animate over to it.
        func sync(_ pager: UIPageViewController) {
            guard !gestureActive, !settling else { return }
            let visible = visibleIndex(of: pager)
            guard visible != parent.index,
                  let target = frontController(at: parent.index) else { return }
            // visible == nil: the page on screen left the sequence (server-side
            // seq rewrite) — jump without animation rather than turning from a
            // ghost. Otherwise animate the same turn a finger drag would.
            guard let visible else {
                pager.setViewControllers([target], direction: .forward, animated: false)
                return
            }
            guard let leaf = animatedLeaf(at: parent.index) else { return }
            settling = true
            let direction: UIPageViewController.NavigationDirection =
                parent.index > visible ? .forward : .reverse
            pager.setViewControllers(leaf, direction: direction, animated: true) {
                [weak self, weak pager] _ in
                DispatchQueue.main.async {
                    self?.settling = false
                    if let pager { self?.sync(pager) } // index moved again mid-flight
                }
            }
        }

        /// The document screen has an explicit back chevron; the navigation
        /// stack's edge-swipe pop must never race a backward page turn.
        func neuterPopGesture(around pager: UIPageViewController) {
            var ancestor: UIViewController? = pager
            while let current = ancestor {
                if let nav = current as? UINavigationController {
                    nav.interactivePopGestureRecognizer?.isEnabled = false
                }
                ancestor = current.parent
            }
        }

        // MARK: data source

        func pageViewController(_ pager: UIPageViewController,
                                viewControllerBefore viewController: UIViewController) -> UIViewController? {
            guard let role = role(of: viewController) else { return nil }
            switch (parent.style, role) {
            case (.slide, .front(let i)): return frontController(at: i - 1)
            case (.curl, .front(let i)): return backController(at: i - 1)
            case (.curl, .back(let i)): return frontController(at: i)
            case (.slide, .back): return nil
            }
        }

        func pageViewController(_ pager: UIPageViewController,
                                viewControllerAfter viewController: UIViewController) -> UIViewController? {
            guard let role = role(of: viewController) else { return nil }
            switch (parent.style, role) {
            case (.slide, .front(let i)): return frontController(at: i + 1)
            case (.curl, .front(let i)): return backController(at: i)
            case (.curl, .back(let i)): return frontController(at: i + 1)
            case (.slide, .back): return nil
            }
        }

        // MARK: delegate

        func pageViewController(_ pager: UIPageViewController,
                                willTransitionTo pendingViewControllers: [UIViewController]) {
            gestureActive = true
        }

        func pageViewController(_ pager: UIPageViewController,
                                didFinishAnimating finished: Bool,
                                previousViewControllers: [UIViewController],
                                transitionCompleted completed: Bool) {
            gestureActive = false
            guard completed, let visible = visibleIndex(of: pager),
                  visible != parent.index else { return }
            parent.index = visible
        }

        // MARK: gesture gating

        func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
            guard parent.edgeOnlyCurl, let view = gestureRecognizer.view else { return true }
            let x = gestureRecognizer.location(in: view).x
            let grab: CGFloat = 56
            return x < grab || x > view.bounds.width - grab
        }
    }
}
