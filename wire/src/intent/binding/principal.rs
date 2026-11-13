//! The principal family's rows of the command binding partitions.

use super::{Binding, BodyShape, CommandKind, Intent};

impl CommandKind {
    pub(super) const fn principal_binding(self) -> Binding {
        use super::AnswerShape as A;
        use BodyShape as B;

        match self {
            Self::PrincipalDirectoryRead => {
                Binding::of(B::PrincipalDirectoryRead, A::PrincipalDirectory)
            }
            Self::PrincipalRead => Binding::of(B::PrincipalRead, A::Principals),
            Self::PrincipalAdmit => Binding::of(B::PrincipalAdmit, A::Principal),
            Self::PrincipalEdit => Binding::of(B::PrincipalEdit, A::Principal),
            Self::PrincipalGrant | Self::PrincipalRevoke => {
                Binding::of(B::PrincipalRoleRef, A::Principal)
            }
            Self::PrincipalDeauthorize => Binding::of(B::PrincipalDeauthorize, A::Principal),
            _ => panic!("principal command binding partition is closed"),
        }
    }
}

impl Intent {
    pub(super) const fn principal_shape(&self) -> BodyShape {
        match self {
            Self::PrincipalDirectoryRead(_) => BodyShape::PrincipalDirectoryRead,
            Self::PrincipalRead(_) => BodyShape::PrincipalRead,
            Self::PrincipalAdmit(_) => BodyShape::PrincipalAdmit,
            Self::PrincipalEdit(_) => BodyShape::PrincipalEdit,
            Self::PrincipalRoleRef(_) => BodyShape::PrincipalRoleRef,
            Self::PrincipalDeauthorize(_) => BodyShape::PrincipalDeauthorize,
            _ => panic!("principal intent shape partition is closed"),
        }
    }
}

impl BodyShape {
    pub(super) fn decode_principal_body(self, bytes: &[u8]) -> Result<Intent, serde_json::Error> {
        Ok(match self {
            Self::PrincipalDirectoryRead => Intent::PrincipalDirectoryRead(crate::decode(bytes)?),
            Self::PrincipalRead => Intent::PrincipalRead(crate::decode(bytes)?),
            Self::PrincipalAdmit => Intent::PrincipalAdmit(crate::decode(bytes)?),
            Self::PrincipalEdit => Intent::PrincipalEdit(crate::decode(bytes)?),
            Self::PrincipalRoleRef => Intent::PrincipalRoleRef(crate::decode(bytes)?),
            Self::PrincipalDeauthorize => Intent::PrincipalDeauthorize(crate::decode(bytes)?),
            _ => unreachable!("principal body partition is closed"),
        })
    }
}
