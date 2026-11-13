macro_rules! row {
    (
        [$($path:literal),+], $syntax:literal, $principal:ident,
        $journal:ident => $position:ident,
        [$($fact:ident),*], [$($guard:ident),*], $tools:expr, $result:ident, $battery:literal
    ) => {
        Contract {
            path: &[$($path),+],
            syntax: $syntax,
            principal: Principal::$principal,
            journal: JournalAccess::$journal,
            reply_position: ReplyPositionLaw::$position,
            facts: &[$(FactKind::$fact),*],
            guards: &[$(Guard::$guard),*],
            tools: $tools,
            result: ResultClass::$result,
            outcomes: ExitOutcome::ALL,
            battery: $battery,
        }
    };
    (
        [$($path:literal),+], $syntax:literal, $principal:ident,
        $journal:ident => $position:ident,
        [$($fact:ident),*], [$($guard:ident),*], $result:ident, $battery:literal
    ) => {
        Contract {
            path: &[$($path),+],
            syntax: $syntax,
            principal: Principal::$principal,
            journal: JournalAccess::$journal,
            reply_position: ReplyPositionLaw::$position,
            facts: &[$(FactKind::$fact),*],
            guards: &[$(Guard::$guard),*],
            tools: NO_TOOLS,
            result: ResultClass::$result,
            outcomes: ExitOutcome::ALL,
            battery: $battery,
        }
    };
    (
        [$($path:literal),+], $syntax:literal, $principal:ident, $journal:ident,
        [$($fact:ident),*], [$($guard:ident),*], $result:ident, $battery:literal
    ) => {
        row!(
            [$($path),+], $syntax, $principal, $journal,
            [$($fact),*], [$($guard),*], NO_TOOLS, $result, $battery
        )
    };
    (
        [$($path:literal),+], $syntax:literal, $principal:ident, $journal:ident,
        [$($fact:ident),*], [$($guard:ident),*], $tools:expr, $result:ident, $battery:literal
    ) => {
        Contract {
            path: &[$($path),+],
            syntax: $syntax,
            principal: Principal::$principal,
            journal: JournalAccess::$journal,
            reply_position: JournalAccess::$journal.reply_position(),
            facts: &[$(FactKind::$fact),*],
            guards: &[$(Guard::$guard),*],
            tools: $tools,
            result: ResultClass::$result,
            outcomes: ExitOutcome::ALL,
            battery: $battery,
        }
    };
}

macro_rules! operator {
    (
        [$($path:literal),+], $syntax:literal, $journal:ident,
        [$($fact:ident),*], $result:ident, $battery:literal
    ) => {
        row!(
            [$($path),+], $syntax, Principal, $journal,
            [$($fact),*], [Resident, OperatorCapability], $result, $battery
        )
    };
    (
        [$($path:literal),+], $syntax:literal, $journal:ident => $position:ident,
        [$($fact:ident),*], $result:ident, $battery:literal
    ) => {
        row!(
            [$($path),+], $syntax, Principal, $journal => $position,
            [$($fact),*], [Resident, OperatorCapability], NO_TOOLS, $result, $battery
        )
    };
}

macro_rules! local_direct {
    (
        [$($path:literal),+], $syntax:literal,
        [$($guard:ident),*], $result:ident, $battery:literal
    ) => {
        row!(
            [$($path),+], $syntax, Local, None,
            [], [$($guard),*], $result, $battery
        )
    };
}
